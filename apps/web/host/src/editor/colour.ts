/**
 * The colours a picker offers.
 *
 * ## Why the picker works in the page's own numbers
 *
 * A colour in a PDF is three numbers between 0 and 1 — the fill colour of
 * the text, the fill colour of a shape — and the page's y axis points up.
 * The picker therefore speaks in the SAME numbers: what the reader picks is
 * what the engine is asked to draw, with no conversion in between, so what
 * the reader sees on the page is what the reader chose.
 *
 * ## The colours that are always there
 *
 * The classic four — black, blue, red, green — are the colours every other
 * editor offers, and they are the first row of the picker: a reader reaching
 * for a colour expects them, not a spectrum.
 */

/** One colour, in the page's own numbers. */
export interface Colour {
	/** The red, between 0 and 1. */
	readonly red: number;
	/** The green, between 0 and 1. */
	readonly green: number;
	/** The blue, between 0 and 1. */
	readonly blue: number;
}

/** The colour as the browser draws it: `#rrggbb`. */
export function toHex(colour: Colour): string {
	const part = (value: number): string =>
		Math.round(Math.min(1, Math.max(0, value)) * 255)
			.toString(16)
			.padStart(2, "0");
	return `#${part(colour.red)}${part(colour.green)}${part(colour.blue)}`;
}

/**
 * The colour from a hex string.
 *
 * The three forms a colour picker offers: \`#rgb\`, \`#rrggbb\` and the same
 * without the hash. Anything else is \`null\`, and the picker keeps what it
 * had.
 */
export function fromHex(text: string): Colour | null {
	const hex = text.trim().replace(/^#/, "");
	if (/^[0-9a-f]{3}$/i.test(hex)) {
		const [r, g, b] = [...hex];
		return {
			red: Number.parseInt(`${r}${r}`, 16) / 255,
			green: Number.parseInt(`${g}${g}`, 16) / 255,
			blue: Number.parseInt(`${b}${b}`, 16) / 255,
		};
	}
	if (/^[0-9a-f]{6}$/i.test(hex)) {
		return {
			red: Number.parseInt(hex.slice(0, 2), 16) / 255,
			green: Number.parseInt(hex.slice(2, 4), 16) / 255,
			blue: Number.parseInt(hex.slice(4, 6), 16) / 255,
		};
	}
	return null;
}

/**
 * The colour at a point of the picker's square.
 *
 * The square is the saturation and the brightness: the x is the saturation
 * (none at the left, full at the right) and the y is the brightness (full
 * at the top, none at the bottom). The hue is the square's own.
 */
export function fromSquare(
	hue: number,
	x: number,
	y: number,
): Colour {
	const saturation = Math.min(1, Math.max(0, x));
	const brightness = Math.min(1, Math.max(0, 1 - y));
	return fromHsv(hue, saturation, brightness);
}

/**
 * The colour on the hue strip.
 *
 * The strip is the hue at full saturation and full brightness, so the reader
 * can see the pure colour they are reaching for.
 */
export function fromHue(hue: number): Colour {
	return fromHsv(hue, 1, 1);
}

/** The colour, in the page's numbers, from hue, saturation and brightness. */
export function fromHsv(hue: number, saturation: number, brightness: number): Colour {
	const h = ((hue % 360) + 360) % 360;
	const c = brightness * saturation;
	const x = c * (1 - Math.abs(((h / 60) % 2) - 1));
	const m = brightness - c;
	let r = 0;
	let g = 0;
	let b = 0;
	if (h < 60) {
		r = c;
		g = x;
	} else if (h < 120) {
		r = x;
		g = c;
	} else if (h < 180) {
		g = c;
		b = x;
	} else if (h < 240) {
		g = x;
		b = c;
	} else if (h < 300) {
		r = x;
		b = c;
	} else {
		r = c;
		b = x;
	}
	return { red: r + m, green: g + m, blue: b + m };
}

/**
 * Where a colour sits on the picker's square and strip.
 *
 * The picker needs to put the mark where the colour is, and it needs the
 * same numbers back when the reader drags the mark.
 */
export function toHsv(colour: Colour): {
	readonly hue: number;
	readonly saturation: number;
	readonly brightness: number;
} {
	const max = Math.max(colour.red, colour.green, colour.blue);
	const min = Math.min(colour.red, colour.green, colour.blue);
	const delta = max - min;
	let hue = 0;
	if (delta > 0) {
		if (max === colour.red) {
			hue = 60 * (((colour.green - colour.blue) / delta) % 6);
		} else if (max === colour.green) {
			hue = 60 * ((colour.blue - colour.red) / delta + 2);
		} else {
			hue = 60 * ((colour.red - colour.green) / delta + 4);
		}
	}
	if (hue < 0) {
		hue += 360;
	}
	return {
		hue,
		saturation: max === 0 ? 0 : delta / max,
		brightness: max,
	};
}

/**
 * The colours the picker always offers.
 *
 * The classic four, the way every editor offers them: black, blue, red and
 * green.
 */
export const CLASSIC_COLOURS: readonly { readonly name: string; readonly colour: Colour }[] = [
	{ name: "Black", colour: { red: 0.05, green: 0.05, blue: 0.12 } },
	{ name: "Blue", colour: { red: 0, green: 0, blue: 1 } },
	{ name: "Red", colour: { red: 1, green: 0, blue: 0 } },
	{ name: "Green", colour: { red: 0, green: 0.5, blue: 0 } },
];
