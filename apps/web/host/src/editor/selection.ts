/**
 * The reader's selection over a page's text.
 *
 * ## Why the browser's own selection is not enough
 *
 * The text layer is a scaffold: its words are transparent, and the reader
 * sees the page's own ink through them. A selection made by the browser
 * paints the **browser's** idea of the text's box — the metrics of the
 * workspace's own sans, at the size the overlay happens to be — so the
 * highlight sits above the words it covers and below them again, and the
 * reader cannot tell what they have picked out.
 *
 * The engine, on the other hand, knows where every character is: the text
 * layer carries each character's box, in the page's own points, around the
 * baseline the glyph was drawn on. A selection drawn from THAT is aligned
 * with the ink by construction, because it is the ink's own geometry.
 *
 * ## What the selection is
 *
 * A start and an end, each a line and a character, and the text between
 * them. The reading order is the layer's order, so a selection that runs
 * backwards is the same text as one that runs forwards — the way every
 * reader expects it.
 */

/** One character's box, in the page's own points, around its baseline. */
export interface CharacterBox {
	/** The box's left edge. */
	readonly x: number;
	/** The box's top edge. */
	readonly y: number;
	/** The box's width. */
	readonly width: number;
	/** The box's height — twice the character's type size. */
	readonly height: number;
	/** Whether the character has ink: a space does not. */
	readonly inked: boolean;
}

/** One line of the text layer, with the characters it holds. */
export interface SelectionLine {
	/** The line's text. */
	readonly text: string;
	/** The line's characters, in reading order. */
	readonly chars: readonly CharacterBox[];
}

/** Where a selection starts or ends: a line and a character in it. */
export interface SelectionEnd {
	/** The zero-based line. */
	readonly line: number;
	/** The zero-based character in that line. */
	readonly char: number;
}

/** The order two ends are in: which one comes first in the reading order. */
export function compareEnds(
	left: SelectionEnd,
	right: SelectionEnd,
): number {
	if (left.line !== right.line) {
		return left.line - right.line;
	}
	return left.char - right.char;
}

/**
 * The character of a line that covers a point.
 *
 * The point is in the page's own points. A character is picked when the
 * point is inside its box, or — when it is not — when it is the nearest
 * one on the line the point is on, so a click just past the end of a word
 * still picks the word.
 */
export function characterAt(
	lines: readonly SelectionLine[],
	point: { readonly x: number; readonly y: number },
): SelectionEnd | null {
	let best: SelectionEnd | null = null;
	let bestDistance = Number.POSITIVE_INFINITY;
	for (let line = 0; line < lines.length; line += 1) {
		const row = lines[line];
		if (row === undefined) {
			continue;
		}
		for (let char = 0; char < row.chars.length; char += 1) {
			const box = row.chars[char];
			if (box === undefined) {
				continue;
			}
			if (
				point.x >= box.x &&
				point.x <= box.x + box.width &&
				point.y >= box.y &&
				point.y <= box.y + box.height
			) {
				return { line, char };
			}
			// The nearest character on the line the point is on.
			const centre = box.y + box.height / 2;
			if (Math.abs(point.y - centre) > box.height) {
				continue;
			}
			const distance = Math.abs(point.x - (box.x + box.width / 2));
			if (distance < bestDistance) {
				bestDistance = distance;
				best = { line, char };
			}
		}
	}
	return best;
}

/**
 * The text between two ends, in reading order.
 *
 * The ends may be given in either order: a selection dragged backwards is
 * the same selection as one dragged forwards.
 */
export function textBetween(
	lines: readonly SelectionLine[],
	from: SelectionEnd,
	to: SelectionEnd,
): string {
	const [start, end] =
		compareEnds(from, to) <= 0 ? [from, to] : [to, from];
	const out: string[] = [];
	for (let line = start.line; line <= end.line; line += 1) {
		const row = lines[line];
		if (row === undefined) {
			continue;
		}
		const first = line === start.line ? start.char : 0;
		const last = line === end.line ? end.char : row.chars.length - 1;
		for (let char = first; char <= last; char += 1) {
			const index = row.text.charCodeAt(char);
			if (index > 0) {
				out.push(String.fromCharCode(index));
			}
		}
		if (line !== end.line) {
			out.push(" ");
		}
	}
	return out.join("");
}

/**
 * The word around a character.
 *
 * A double-click picks the word, the way every editor does it: the word is
 * the run of characters between the spaces, and the spaces themselves are not
 * part of it.
 */
export function wordAround(
	lines: readonly SelectionLine[],
	at: SelectionEnd,
): { readonly from: SelectionEnd; readonly to: SelectionEnd } | null {
	const row = lines[at.line];
	if (row === undefined) {
		return null;
	}
	const isSpace = (index: number): boolean => {
		const code = row.text.charCodeAt(index);
		return code === 32 || code === 9 || code === 10 || code === 13;
	};
	let start = at.char;
	while (start > 0 && !isSpace(start - 1)) {
		start -= 1;
	}
	let end = at.char;
	while (end + 1 < row.text.length && !isSpace(end + 1)) {
		end += 1;
	}
	if (start === end && start >= row.text.length) {
		return null;
	}
	return {
		from: { line: at.line, char: start },
		to: { line: at.line, char: end },
	};
}

/**
 * The boxes to paint, one run per line.
 *
 * The run is the box the selected characters cover, trimmed to the type's
 * own ascent and descent: the engine's character box is twice the type
 * size tall, and the ink of a line is not.
 */
export function runsToPaint(
	lines: readonly SelectionLine[],
	from: SelectionEnd,
	to: SelectionEnd,
): readonly {
	readonly line: number;
	readonly x: number;
	readonly y: number;
	readonly width: number;
	readonly height: number;
}[] {
	const [start, end] = compareEnds(from, to) <= 0 ? [from, to] : [to, from];
	const out: {
		line: number;
		x: number;
		y: number;
		width: number;
		height: number;
	}[] = [];
	for (let line = start.line; line <= end.line; line += 1) {
		const row = lines[line];
		if (row === undefined) {
			continue;
		}
		const first = line === start.line ? start.char : 0;
		const last = line === end.line ? end.char : row.chars.length - 1;
		const boxes = row.chars.slice(first, last + 1);
		const inked = boxes.filter((box) => box.inked);
		const source = inked.length > 0 ? inked : boxes;
		if (source.length === 0) {
			continue;
		}
		let left = Number.POSITIVE_INFINITY;
		let right = Number.NEGATIVE_INFINITY;
		let top = Number.POSITIVE_INFINITY;
		let bottom = Number.NEGATIVE_INFINITY;
		for (const box of source) {
			left = Math.min(left, box.x);
			right = Math.max(right, box.x + box.width);
			top = Math.min(top, box.y);
			bottom = Math.max(bottom, box.y + box.height);
		}
		if (!Number.isFinite(left) || !Number.isFinite(top)) {
			continue;
		}
		// The ink of a line sits between the descent and the ascent: a
		// quarter of the type below the baseline, three quarters above it.
		//
		// The page's y axis points UP, so a box drawn from `y` upwards
		// reaches the ASCENT: the box starts a quarter of the type below the
		// baseline and ends three quarters above it.
		const size = (bottom - top) / 2;
		const baseline = top + size;
		out.push({
			line,
			x: left,
			y: baseline - size * 0.25,
			width: right - left,
			height: size,
		});
	}
	return out;
}
