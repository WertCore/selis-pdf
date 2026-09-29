/**
 * The text layer's geometry (SL-4.UI.04), half one: **where every character is,
 * in CSS pixels, inside a page box**.
 *
 * The engine's `PageTextLayer` is in PDF user space — points, y-up, origin at the
 * MediaBox's bottom-left corner. The browser is CSS pixels, y-down, origin at
 * the top-left of the positioned page element. This module is the *only* place
 * that conversion happens, because it is the single place it can be got wrong
 * twice: the y-flip and the scale factor are both silent when wrong. The glyphs
 * still look right underneath (they are painted by the engine, not by us), so
 * a selection that is vertically mirrored or half-size is visible only as
 * "selection highlights the wrong words" — a bug that no type error and no
 * golden test on the *engine* half would ever catch.
 *
 * ## Which scale, and which scale is a trap
 *
 * The one number this multiplies by is `PlacedPage.scale` — **CSS pixels per
 * PDF point**, the value `layout.ts` used to place the box. That is neither of
 * the two device-pixel numbers, and confusing it with either is the bug this
 * comment exists to prevent:
 *
 * - `SurfaceSize.devicePixelRatio` is the *nominal* ratio. A zoomed browser at
 *   125% on a fractional display reports 1.25.
 * - `SurfaceSize.scale` is the *real* backing-store factor, `deviceWidth /
 *   cssWidth` — at DPR 1.25 a 1013-CSS-px viewport is 1266 device px, so the
 *   real factor is 1.2497…. UI.03 had to compute that number to avoid a
 *   half-pixel seam in the canvas.
 *
 * Neither belongs here, and neither does the tile's `deviceScale`. The text
 * layer is DOM, laid out by the engine's own CSS box model in CSS pixels; the
 * browser applies the device factor itself, once, when it rasterises.
 * Multiplying by a device ratio here would place the text layer at 1.25× its
 * box on a 125% display and the selection would drift off the glyphs — while
 * still looking plausible, because the drift is proportional.
 *
 * ## Zoom survival is structural, not a feature
 *
 * A selection is stored as `(line, utf16Offset)` pairs and the quads are
 * re-projected on every zoom, from the same scale-free user-space layer. Nothing
 * here is a pixel, so a selection made at 1× and re-read at 8.3× cannot go
 * stale: there is no cached pixel rectangle to go stale.
 *
 * ## The seam
 *
 * This module is pure arithmetic over plain data and touches no platform
 * global — no `OffscreenCanvas`, no `Worker`, no `requestAnimationFrame` — so it
 * lives in the host-agnostic package and `../platform/platform-globals.test.ts` stays green.
 * It also paints nothing. Like `compositor.ts` handing a `CompositorFrame` to a
 * `TileSurface`, this produces a `TextLayerFrame` that the host renders as
 * ordinary DOM. That is what makes the DoD testable at all: the repo ships no
 * jsdom, and holding no platform global is what keeps it that way
 * (SL-4.UI.01 / ADR-P0044),
 * so the layer has to be assertable as numbers rather than only as pixels on a
 * screen nobody has.
 */

import type { PageTextLayer, TextLayerLine } from "../platform/types.js";
import type { PlacedPage } from "./layout.js";

/** A CSS-pixel box, origin at the top-left of the page element, y down. */
export interface CharBox {
	readonly x: number;
	readonly y: number;
	readonly width: number;
	readonly height: number;
}

/** One line, positioned, with its characters positioned inside it. */
export interface LayerLineBox {
	/** The line's text. Index-aligned with `chars`, in reading order. */
	readonly text: string;
	/** The writing direction the engine resolved from the quads. */
	readonly direction: "ltr" | "rtl";
	/** The line's box, in CSS pixels relative to the page element. */
	readonly box: CharBox;
	/**
	 * One entry per UTF-16 code unit of `text`, in order. The engine guarantees
	 * the alignment; this layer does not re-derive or re-order it.
	 */
	readonly chars: readonly CharBox[];
	/**
	 * Which code-unit indices are real glyphs. A synthesised inter-word space
	 * is not, and a caret must be able to sit on either side of it while a
	 * highlight over it covers the gap it stands for.
	 */
	readonly inked: readonly boolean[];
}

/**
 * Project a PDF-space rectangle into the page element's CSS-pixel box.
 *
 * The flip: user-space `y` counts up from the MediaBox bottom, CSS `y` counts
 * down from the element top, so the box's *top* edge is the distance from the
 * page's top down to the box's top edge — `pageHeight - (y + height)` — and not
 * `y`, and not `pageHeight - y`. Getting this to `pageHeight - y` is the
 * classic error: it happens to be right for a zero-height box and wrong by the
 * full box height for every real one.
 */
function projectBox(
	box: { x: number; y: number; width: number; height: number },
	pageHeightPoints: number,
	scale: number,
): CharBox {
	return {
		x: box.x * scale,
		y: (pageHeightPoints - (box.y + box.height)) * scale,
		width: box.width * scale,
		height: box.height * scale,
	};
}

function projectLine(line: TextLayerLine, pageHeightPoints: number, scale: number): LayerLineBox {
	const chars: CharBox[] = [];
	const inked: boolean[] = [];
	for (const char of line.chars) {
		chars.push(projectBox(char.rect, pageHeightPoints, scale));
		inked.push(char.inked);
	}
	return {
		text: line.text,
		direction: line.direction,
		box: projectBox(line.rect, pageHeightPoints, scale),
		chars,
		inked,
	};
}

/**
 * Position a page's text layer into the box `placed` describes.
 *
 * `placed.scale` is the whole of the scale question (see the module doc). The
 * page's own `height` in points is the y-flip's reference, and it comes from
 * the same `textLayer` call as the quads rather than from a separate metadata
 * request, which could disagree with them.
 *
 * Pure, and the only constructor of a {@link TextLayerFrame}.
 */
export function buildTextLayer(layer: PageTextLayer, placed: PlacedPage): TextLayerFrame {
	const scale = placed.scale;
	const pageHeightPoints = layer.height;
	return {
		page: layer.page,
		width: placed.width,
		height: placed.height,
		scale,
		lines: layer.lines.map((line) => projectLine(line, pageHeightPoints, scale)),
		text: layer.text,
		lowConfidence: layer.lowConfidence,
	};
}

/** The number of UTF-16 code units on a line, which is its last valid caret. */
export function lineLength(frame: TextLayerFrame, line: number): number {
	return frame.lines[line]?.chars.length ?? 0;
}

/**
 * The visual x of a caret, in CSS pixels — where a caret is drawn and what a
 * vertical move tries to keep constant.
 *
 * This is where direction is most visible. A left-to-right line's caret at
 * offset `o` sits at the *left* edge of character `o`; a right-to-left line's
 * sits at the *right* edge, because the pen started at the right and walked
 * left. A caller that used one of these for both would put every caret on an
 * RTL page half a line away from where the reader clicked.
 *
 * An offset past the end of the line clamps to the last character's trailing
 * edge, so the caret at the end of a line is stable rather than off the box.
 */
export function caretX(frame: TextLayerFrame, line: number, offset: number): number {
	const chars = frame.lines[line]?.chars;
	if (chars === undefined || chars.length === 0) {
		return 0;
	}
	const rtl = frame.lines[line]?.direction === "rtl";
	const last = chars[chars.length - 1];
	if (last === undefined) {
		return 0;
	}
	const endEdge = rtl ? last.x : last.x + last.width;
	if (offset >= chars.length) {
		return endEdge;
	}
	const box = chars[Math.max(0, offset)];
	if (box === undefined) {
		return endEdge;
	}
	// Offset 0 is the line's logical start: the leading edge in the writing
	// direction, which is the *right* edge of character 0 on an RTL line.
	return rtl ? box.x + box.width : box.x;
}

export interface TextLayerFrame {
	readonly page: number;
	/** The page element's size in CSS pixels — the frame is sized to it. */
	readonly width: number;
	readonly height: number;
	/** The scale this frame was projected at, CSS pixels per PDF point. */
	readonly scale: number;
	/** Lines in **reading** order, not visual order. */
	readonly lines: readonly LayerLineBox[];
	/**
	 * The page's text in reading order, byte-identical to what
	 * `selis extract --format=text` prints for the page. Carried through from
	 * the engine rather than re-assembled here — `copyText` slices this, and a
	 * second joiner in this package is exactly the drift the DoD is about.
	 */
	readonly text: string;
	/** SL-3.TEXT.10: text was drawn but nothing recovered. */
	readonly lowConfidence: boolean;
}
