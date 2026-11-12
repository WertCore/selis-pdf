/**
 * Measuring on the page.
 *
 * Acrobat and Foxit both have a measure tool, and both draw the measurement
 * on the screen WITHOUT putting it in the document: it is a reading, not an
 * edit. This module is the arithmetic, and it is pure, because the
 * arithmetic is the part worth testing.
 *
 * The page's own space is **points**, y up, with the origin at the lower
 * left of the media box. The canvas is y down. The angle therefore flips:
 * a line that goes DOWN the canvas goes DOWN the page too, but the page's
 * angle is measured the other way round.
 */

/** One measurement. */
export interface Measurement {
	/** The distance, in points. */
	readonly points: number;
	/** The distance, in millimetres. */
	readonly millimetres: number;
	/** The angle from the page's horizontal, in degrees, 0 to 360. */
	readonly degrees: number;
}

/** A point in the page's own space. */
export interface MeasurePoint {
	readonly x: number;
	readonly y: number;
}

/** How many millimetres a point is. */
const MM_PER_POINT = 25.4 / 72;

/**
 * The distance and the angle between two points.
 *
 * The angle is measured from the page's horizontal, counterclockwise, the
 * way a protractor measures it, and it is never negative: a line that goes
 * down and to the left is at 200 degrees, not at -160.
 */
export function measureBetween(from: MeasurePoint, to: MeasurePoint): Measurement {
	const dx = to.x - from.x;
	const dy = to.y - from.y;
	const points = Math.hypot(dx, dy);
	const degrees = normalise((Math.atan2(dy, dx) * 180) / Math.PI);
	return {
		points,
		millimetres: points * MM_PER_POINT,
		degrees,
	};
}

/**
 * The measurement in the page's space, from a drag on the canvas.
 *
 * The canvas is y down and the page is y up, so the drag's vertical
 * distance changes sign on the way in. The scale is the page's own: how
 * many CSS pixels a point is on THIS page, at THIS zoom.
 */
export function measureOnCanvas(
	from: { readonly x: number; readonly y: number },
	to: { readonly x: number; readonly y: number },
	cssPerPoint: number,
): Measurement {
	if (!(cssPerPoint > 0)) {
		return { points: 0, millimetres: 0, degrees: 0 };
	}
	return measureBetween(
		{ x: from.x / cssPerPoint, y: -from.y / cssPerPoint },
		{ x: to.x / cssPerPoint, y: -to.y / cssPerPoint },
	);
}

/** An angle in degrees, as a number from 0 to 360. */
function normalise(degrees: number): number {
	const wrapped = degrees % 360;
	return wrapped < 0 ? wrapped + 360 : wrapped;
}

/**
 * The measurement, as the reader is shown it.
 *
 * Both units, because a reader's ruler is in millimetres or inches and the
 * page's own unit is the point: "128.4 pt (45.3 mm)" answers either
 * question without the reader doing the arithmetic.
 */
export function measurementText(measurement: Measurement): string {
	const points = measurement.points.toFixed(1);
	const millimetres = measurement.millimetres.toFixed(1);
	return `${points} pt (${millimetres} mm) · ${measurement.degrees.toFixed(1)}°`;
}
