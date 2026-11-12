/**
 * The annotations the workspace knows about, on the page it is showing.
 *
 * The engine's report gives their rectangles in the page's own points; the
 * editor needs to know where they are ON THE SCREEN, so that a reader can
 * pick one up and move it.
 */

/** One annotation, in the editor's own terms. */
export interface EditorAnnotation {
	/** The annotation's position in the page's `/Annots` order. */
	readonly index: number;
	/** Its rectangle, in the canvas's CSS pixels. */
	readonly rect: { x: number; y: number; width: number; height: number };
	/** What it says, when it says anything. */
	readonly contents: string;
	/** What kind of mark it is. */
	readonly subtype: string;
}

/**
 * The annotation under a point, if there is one.
 *
 * The LAST annotation under the point wins: an annotation drawn later is on
 * top, and the reader means the one they can see.
 */
export function annotationAt(
	annotations: readonly EditorAnnotation[],
	x: number,
	y: number,
): EditorAnnotation | null {
	let found: EditorAnnotation | null = null;
	for (const annotation of annotations) {
		const { rect } = annotation;
		if (x >= rect.x && x <= rect.x + rect.width && y >= rect.y && y <= rect.y + rect.height) {
			found = annotation;
		}
	}
	return found;
}
