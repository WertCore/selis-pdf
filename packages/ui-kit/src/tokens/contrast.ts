/**
 * WCAG 2.x contrast math over sRGB hex colours. Kept here (not only in tests)
 * because canvas-drawn UI — tile placeholders, page-number plates, overlay
 * chrome — needs the same gate at runtime that CI applies to the tokens.
 */

const HEX_PATTERN = /^#[0-9a-fA-F]{6}$/;

/** Throw a descriptive error for a malformed token value. */
function parseHex(hex: string): readonly [number, number, number] {
	if (!HEX_PATTERN.test(hex)) {
		throw new Error(`colour token must be #rrggbb, got "${hex}"`);
	}
	const r = Number.parseInt(hex.slice(1, 3), 16);
	const g = Number.parseInt(hex.slice(3, 5), 16);
	const b = Number.parseInt(hex.slice(5, 7), 16);
	return [r, g, b];
}

/** One sRGB channel (0–255) linearised per WCAG 2.x. */
function linearise(channel: number): number {
	const c = channel / 255;
	return c <= 0.04045 ? c / 12.92 : ((c + 0.055) / 1.055) ** 2.4;
}

/** WCAG 2.x relative luminance of a `#rrggbb` colour. */
export function relativeLuminance(hex: string): number {
	const [r, g, b] = parseHex(hex);
	return 0.2126 * linearise(r) + 0.7152 * linearise(g) + 0.0722 * linearise(b);
}

/** WCAG 2.x contrast ratio between two `#rrggbb` colours (1–21). */
export function contrastRatio(a: string, b: string): number {
	const la = relativeLuminance(a);
	const lb = relativeLuminance(b);
	const lighter = Math.max(la, lb);
	const darker = Math.min(la, lb);
	return (lighter + 0.05) / (darker + 0.05);
}
