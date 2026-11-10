/**
 * Where a placement lands: the geometry between the canvas and the two spaces
 * the engine's writers use.
 *
 * ## Three spaces, and why there are three
 *
 * - **CSS pixels** are what the reader drags on. The canvas is one of them.
 * - **User points** are the page's own coordinate system: the `/MediaBox`
 *   space, points, y-up, origin at the media box's lower-left corner. The
 *   redaction edits the content stream, so its rectangles are in this space.
 * - **Display points** are the page as it is *displayed* — the `/Rotate`
 *   applied — with the origin at the bottom-left of what the reader sees. The
 *   stamp and the image overlay are placed in this space, because the engine
 *   draws them relative to the displayed page.
 *
 * The engine's own transform connects the first two: the render reports the
 * matrix it used, so user points are the inverse of that matrix applied to the
 * canvas. Nothing here guesses.
 *
 * The display space is reached from the user space through the engine's own
 * quarter-turn table — the same one the overlay writers use — so a page that is
 * turned on its side, or whose media box does not start at (0, 0), is placed
 * where the reader pointed and not where a guess would put it.
 */
import type { Matrix, Point } from "./geometry.js";

/** A rectangle, in whatever space the caller is working in. */
export interface Rect {
	readonly x: number;
	readonly y: number;
	readonly width: number;
	readonly height: number;
}

/**
 * The inverse of a matrix.
 *
 * `None` when the matrix is singular, which a render's transform never is: it
 * is a scale by a positive number of pixels per point.
 */
export function invert(m: Matrix): Matrix | null {
	const det = m.a * m.d - m.b * m.c;
	if (!Number.isFinite(det) || det === 0) {
		return null;
	}
	const a = m.d / det;
	const b = -m.b / det;
	const c = -m.c / det;
	const d = m.a / det;
	return {
		a,
		b,
		c,
		d,
		e: -(a * m.e + c * m.f),
		f: -(b * m.e + d * m.f),
	};
}

/** Apply a matrix to a point. */
export function applyMatrix(m: Matrix, x: number, y: number): Point {
	return { x: m.a * x + m.c * y + m.e, y: m.b * x + m.d * y + m.f };
}

/** The page's `/Rotate`, as the engine's quadrant. */
export type PageRotation = 0 | 90 | 180 | 270;

/** Everything the conversions need about one rendered page. */
export interface PageGeometry {
	/** The transform the render used: user points → device pixels. */
	readonly ctm: Matrix;
	/** The raster's width, in device pixels. */
	readonly rasterWidth: number;
	/** The raster's height, in device pixels. */
	readonly rasterHeight: number;
	/** The canvas's width, in CSS pixels. */
	readonly cssWidth: number;
	/** The canvas's height, in CSS pixels. */
	readonly cssHeight: number;
	/** The media box's width, in points (unrotated). */
	readonly mediaWidth: number;
	/** The media box's height, in points (unrotated). */
	readonly mediaHeight: number;
	/** The page's `/Rotate`. */
	readonly rotation: PageRotation;
	/** The device pixels per point. */
	readonly scale: number;
}

/**
 * Work out a page's geometry from the transform the render reported.
 *
 * The rotation and the media box's origin are both in that transform: the
 * linear part says which way the page is turned, and the translation says
 * where the media box starts.
 */
export function pageGeometry(
	ctm: Matrix,
	rasterWidth: number,
	rasterHeight: number,
	cssWidth: number,
	cssHeight: number,
	mediaWidth: number,
	mediaHeight: number,
): PageGeometry {
	const scale = Math.hypot(ctm.a, ctm.b);
	// The text direction is the image of the user-space x axis, so its angle is
	// the page's rotation.
	const angle = (Math.atan2(ctm.b, ctm.a) * 180) / Math.PI;
	const rotation: PageRotation =
		angle < 45 || angle >= 315 ? 0 : angle < 135 ? 90 : angle < 225 ? 180 : 270;

	return {
		ctm,
		rasterWidth,
		rasterHeight,
		cssWidth,
		cssHeight,
		mediaWidth,
		mediaHeight,
		rotation,
		scale,
	};
}

/**
 * The engine's display-point → media-relative user-point mapping.
 *
 * The same table the overlay writers use: a quarter turn about the media box,
 * with the media box's own size in the translation.
 */
export function displayToUser(
	rotation: PageRotation,
	mediaWidth: number,
	mediaHeight: number,
	x: number,
	y: number,
): Point {
	switch (rotation) {
		case 90:
			return { x: mediaWidth - y, y: x };
		case 180:
			return { x: mediaWidth - x, y: mediaHeight - y };
		case 270:
			return { x: y, y: mediaHeight - x };
		default:
			return { x, y };
	}
}

/**
 * The inverse of [`displayToUser`]: a media-relative user point back to the
 * display point it came from.
 */
export function userToDisplay(
	rotation: PageRotation,
	mediaWidth: number,
	mediaHeight: number,
	x: number,
	y: number,
): Point {
	switch (rotation) {
		case 90:
			return { x: y, y: mediaWidth - x };
		case 180:
			return { x: mediaWidth - x, y: mediaHeight - y };
		case 270:
			return { x: mediaHeight - y, y: x };
		default:
			return { x, y };
	}
}

/** The device pixels a canvas rectangle covers. */
function toDevice(geometry: PageGeometry, rect: Rect): Rect {
	const perX = geometry.rasterWidth / Math.max(1, geometry.cssWidth);
	const perY = geometry.rasterHeight / Math.max(1, geometry.cssHeight);
	return {
		x: rect.x * perX,
		y: rect.y * perY,
		width: rect.width * perX,
		height: rect.height * perY,
	};
}

/**
 * The user-space rectangle a canvas rectangle covers.
 *
 * Exact: it is the inverse of the transform the engine rendered with, so it
 * holds for a turned page and for a media box that does not start at the
 * origin.
 */
export function userRect(geometry: PageGeometry, canvas: Rect): Rect {
	const device = toDevice(geometry, canvas);
	const inverse = invert(geometry.ctm);
	if (inverse === null) {
		return { x: 0, y: 0, width: 0, height: 0 };
	}
	const corners = [
		applyMatrix(inverse, device.x, device.y),
		applyMatrix(inverse, device.x + device.width, device.y),
		applyMatrix(inverse, device.x + device.width, device.y + device.height),
		applyMatrix(inverse, device.x, device.y + device.height),
	];
	let minX = Number.POSITIVE_INFINITY;
	let minY = Number.POSITIVE_INFINITY;
	let maxX = Number.NEGATIVE_INFINITY;
	let maxY = Number.NEGATIVE_INFINITY;
	for (const corner of corners) {
		minX = Math.min(minX, corner.x);
		minY = Math.min(minY, corner.y);
		maxX = Math.max(maxX, corner.x);
		maxY = Math.max(maxY, corner.y);
	}
	return { x: minX, y: minY, width: maxX - minX, height: maxY - minY };
}

/**
 * The display-space rectangle the overlay writers want.
 *
 * The overlay writers take a rectangle in *their* display convention and
 * write the user point it maps to — so the rectangle handed to them is the
 * inverse of that mapping applied to the user rectangle. (Their convention is
 * the media-relative one, so a page whose media box does not start at (0, 0)
 * still lands exactly where the reader pointed: the writer's own arithmetic
 * puts the point back where it came from.)
 */
export function displayRect(
	geometry: PageGeometry,
	canvas: Rect,
): { x0: number; y0: number; x1: number; y1: number } {
	const user = userRect(geometry, canvas);
	const first = userToDisplay(
		geometry.rotation,
		geometry.mediaWidth,
		geometry.mediaHeight,
		user.x,
		user.y,
	);
	const second = userToDisplay(
		geometry.rotation,
		geometry.mediaWidth,
		geometry.mediaHeight,
		user.x + user.width,
		user.y + user.height,
	);
	return {
		x0: Math.min(first.x, second.x),
		y0: Math.min(first.y, second.y),
		x1: Math.max(first.x, second.x),
		y1: Math.max(first.y, second.y),
	};
}

/**
 * The size an image should be drawn at to fill a rectangle, keeping its own
 * aspect ratio.
 *
 * The overlay writer scales the image to the rectangle it is given — it does
 * not preserve the ratio — so the ratio is preserved here, and the image is
 * centred in the rectangle the reader drew.
 */
export function fitImage(
	rect: { x0: number; y0: number; x1: number; y1: number },
	imageWidth: number,
	imageHeight: number,
): { x: number; y: number; width: number; height: number; scale: number } {
	const boxWidth = Math.max(0, rect.x1 - rect.x0);
	const boxHeight = Math.max(0, rect.y1 - rect.y0);
	if (!(imageWidth > 0) || !(imageHeight > 0) || boxWidth <= 0 || boxHeight <= 0) {
		return { x: rect.x0, y: rect.y0, width: 0, height: 0, scale: 1 };
	}
	// One point per pixel is the writer's own default scale, so the image's
	// natural size in points is its pixel size.
	const natural = Math.min(boxWidth / imageWidth, boxHeight / imageHeight);
	const width = imageWidth * natural;
	const height = imageHeight * natural;
	return {
		x: rect.x0 + (boxWidth - width) / 2,
		y: rect.y0 + (boxHeight - height) / 2,
		width,
		height,
		scale: natural,
	};
}
