/**
 * The text styles.
 *
 * ## What this can and cannot do
 *
 * The styles apply to the text the reader ADDS — the text box, and the
 * annotation's own text — and they are the same three the competitors
 * offer: bold, italic and underline.
 *
 * They do NOT apply to the text already in the document, and no editor can
 * make them: a PDF's text is a stream of glyphs drawn by a font the
 * document embeds, so changing the weight of a word means redrawing the
 * page. Acrobat does not offer it either. What Acrobat offers is the
 * formatting bar on the text you are ADDING, and so do we.
 */

/** The styles a line of text can carry. */
export interface TextStyle {
	/** The line is drawn in the bold face. */
	readonly bold: boolean;
	/** The line is drawn in the italic face. */
	readonly italic: boolean;
	/** The line is underlined. */
	readonly underline: boolean;
}

/** The style the editor starts with. */
export const PLAIN_STYLE: TextStyle = { bold: false, italic: false, underline: false };

/** The style with one flag turned on, and the rest as they were. */
export function withStyle(style: TextStyle, flag: keyof TextStyle, on: boolean): TextStyle {
	return { ...style, [flag]: on };
}
