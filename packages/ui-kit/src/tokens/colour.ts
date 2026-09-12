/**
 * Colour tokens — the single source of truth for every colour the UI uses
 * (SL-4.UI.10). Values are sRGB hex so WCAG contrast is computable in tests;
 * `css/tokens.css` is generated from this data and a unit test keeps the two
 * in sync (run `pnpm --filter @selis/ui-kit test -u` after editing here).
 *
 * Rules:
 * - Every value is bound to a *role*, never used raw. Components reference
 *   `var(--selis-color-<role>)` only.
 * - `textDisabled` is the only role exempt from the 4.5:1 contrast gate
 *   (WCAG 1.4.1 disabled-control carve-out); do not use it for anything
 *   the user must read.
 * - High-contrast themes override roles; they must keep every pair that the
 *   base theme passes at its (higher) HC threshold.
 */

export type Hex = string;

/** Every colour role the design system defines. All are required per theme. */
export interface RoleColours {
	/** App background behind everything (the area around pages). */
	bg: Hex;
	/** Panels, toolbars, dialogs, cards. */
	surface: Hex;
	/** Wells and recessed areas inside surfaces. */
	surfaceSunken: Hex;
	/** Primary text on `bg`, `surface`, `surfaceSunken`. */
	text: Hex;
	/** Secondary text: labels, metadata, inactive list items. */
	textSecondary: Hex;
	/** Muted text: hints, captions, page labels. Still ≥ 4.5:1. */
	textMuted: Hex;
	/** Disabled text. Exempt from the contrast gate (WCAG 1.4.1). */
	textDisabled: Hex;
	/** Hairline borders, dividers. Non-text usage. */
	borderSubtle: Hex;
	/** Emphasised borders: input focus frames, dividers on busy art. */
	borderStrong: Hex;
	/** Keyboard focus indicator. Must clear 3:1 (HC: 4.5:1) on `surface`. */
	focusRing: Hex;
	/** Primary action background; `onAccent` text sits on it. */
	accent: Hex;
	/** Hover/pressed state for `accent`. */
	accentHover: Hex;
	/** Quiet accent tint (selected nav items, toggles off-state). */
	accentSoft: Hex;
	/** Text/icons on `accent`, `accentHover`, `selectionBg`. */
	onAccent: Hex;
	/** Link text. ≥ 4.5:1 on `surface`. */
	link: Hex;
	/** Text-selection background (also used by the text layer). */
	selectionBg: Hex;
	/** Text colour over `selectionBg`. */
	selectionText: Hex;
	/** Search-match highlight background under the text layer. */
	highlightBg: Hex;
	/** Current-match highlight background. */
	highlightActiveBg: Hex;
	/** Text colour over both highlight backgrounds. */
	highlightText: Hex;
	/** Solid success accent (progress, badges use the soft pair). */
	success: Hex;
	/** Success badge background; pair with `successText`. */
	successSoft: Hex;
	/** Success text on `successSoft`. */
	successText: Hex;
	/** Solid warning accent. */
	warning: Hex;
	/** Warning badge background; pair with `warningText`. */
	warningSoft: Hex;
	/** Warning text on `warningSoft`. */
	warningText: Hex;
	/** Solid danger accent (destructive buttons; `onAccent` text on it). */
	danger: Hex;
	/** Danger badge background; pair with `dangerText`. */
	dangerSoft: Hex;
	/** Danger text on `dangerSoft`. */
	dangerText: Hex;
	/** Page-tile placeholder while a render resolves. */
	pagePlaceholder: Hex;
	/** Page label text on `pagePlaceholder`. */
	pagePlaceholderText: Hex;
	/** Scrollbar thumb (track uses `surfaceSunken`). */
	scrollbarThumb: Hex;
	/** Elevation shadow colour for `shadow-1`/`shadow-2` compositions. */
	shadow: Hex;
}

/** A resolved (base + optional high-contrast override) theme. */
export interface Theme {
	id: string;
	/** `color-scheme` value emitted into the generated CSS block. */
	colorScheme: "light" | "dark";
	colours: RoleColours;
	/** Elevated surfaces: `--selis-shadow-1`, `--selis-shadow-2`. */
	shadows: { readonly shadow1: string; readonly shadow2: string };
}

const light: RoleColours = {
	bg: "#f2f4f7",
	surface: "#ffffff",
	surfaceSunken: "#e4e8ec",
	text: "#171b1f",
	textSecondary: "#47525c",
	textMuted: "#5d6975",
	textDisabled: "#9aa4ad",
	borderSubtle: "#d3d9df",
	borderStrong: "#8a949e",
	focusRing: "#2557d0",
	accent: "#2b5bc4",
	accentHover: "#234aa8",
	accentSoft: "#dbe5fb",
	onAccent: "#ffffff",
	link: "#1d4fc0",
	selectionBg: "#2b5bc4",
	selectionText: "#ffffff",
	highlightBg: "#ffe066",
	highlightActiveBg: "#ffab40",
	highlightText: "#171b1f",
	success: "#1e7d43",
	successSoft: "#dcf2e3",
	successText: "#1a6b38",
	warning: "#7a4d00",
	warningSoft: "#fdeec8",
	warningText: "#7a4d00",
	danger: "#b3261e",
	dangerSoft: "#fbe2e0",
	dangerText: "#a12118",
	pagePlaceholder: "#dfe3e8",
	pagePlaceholderText: "#47525c",
	scrollbarThumb: "#b8bfc7",
	shadow: "#0f1720",
};

const dark: RoleColours = {
	bg: "#121519",
	surface: "#1d2229",
	surfaceSunken: "#0d1014",
	text: "#e9edf1",
	textSecondary: "#b6c0c9",
	textMuted: "#8f9aa4",
	textDisabled: "#5c6670",
	borderSubtle: "#333b44",
	borderStrong: "#7d8892",
	focusRing: "#8fb4f5",
	accent: "#3667dc",
	accentHover: "#4a78ea",
	accentSoft: "#223252",
	onAccent: "#ffffff",
	link: "#9cbdf3",
	selectionBg: "#3667dc",
	selectionText: "#ffffff",
	highlightBg: "#7a5c00",
	highlightActiveBg: "#965700",
	highlightText: "#e9edf1",
	success: "#35a05f",
	successSoft: "#1d3a28",
	successText: "#63c788",
	warning: "#ecc069",
	warningSoft: "#413410",
	warningText: "#ecc069",
	danger: "#c33c30",
	dangerSoft: "#44201d",
	dangerText: "#f28f85",
	pagePlaceholder: "#232a32",
	pagePlaceholderText: "#b6c0c9",
	scrollbarThumb: "#454f59",
	shadow: "#000000",
};

/** High-contrast overrides on top of the light theme. */
export const lightHighContrast: Partial<RoleColours> = {
	bg: "#fafafa",
	surface: "#ffffff",
	surfaceSunken: "#f0f0f0",
	text: "#000000",
	textSecondary: "#1a1d21",
	textMuted: "#333a41",
	borderSubtle: "#6d7780",
	borderStrong: "#000000",
	focusRing: "#00339e",
	accent: "#123a9e",
	accentHover: "#0e2f82",
	accentSoft: "#dce6ff",
	link: "#00339e",
	selectionBg: "#00339e",
	highlightBg: "#ffd23f",
	highlightActiveBg: "#ff9e1f",
	success: "#0d5c2e",
	successSoft: "#d6f0dd",
	successText: "#0d5c2e",
	warning: "#5f3d00",
	warningSoft: "#ffefc2",
	warningText: "#5f3d00",
	danger: "#a81b12",
	dangerSoft: "#ffd9d5",
	dangerText: "#8f1d15",
	pagePlaceholder: "#eceef1",
	pagePlaceholderText: "#1a1d21",
	scrollbarThumb: "#6d7780",
};

/** High-contrast overrides on top of the dark theme. */
export const darkHighContrast: Partial<RoleColours> = {
	bg: "#000000",
	surface: "#0d1014",
	surfaceSunken: "#000000",
	text: "#ffffff",
	textSecondary: "#dbe3ea",
	textMuted: "#c3ccd4",
	textDisabled: "#6d7780",
	borderSubtle: "#8a949e",
	borderStrong: "#ffffff",
	focusRing: "#bfe3ff",
	accent: "#2451c8",
	accentHover: "#3763dd",
	accentSoft: "#12233f",
	link: "#cfe2ff",
	selectionBg: "#2451c8",
	highlightBg: "#6e5200",
	highlightActiveBg: "#7a4300",
	highlightText: "#ffffff",
	success: "#4cc27a",
	successSoft: "#14301f",
	successText: "#7fe0a2",
	warning: "#ffd07a",
	warningSoft: "#3a2c08",
	warningText: "#ffd07a",
	danger: "#c9372b",
	dangerSoft: "#431815",
	dangerText: "#ffb0a6",
	pagePlaceholder: "#1d2229",
	pagePlaceholderText: "#ffffff",
	scrollbarThumb: "#6d7780",
};

export const baseThemes: Record<"light" | "dark", Theme> = {
	light: {
		id: "light",
		colorScheme: "light",
		colours: light,
		shadows: {
			shadow1: "0 1px 2px rgba(15, 23, 32, 0.08), 0 2px 8px rgba(15, 23, 32, 0.10)",
			shadow2: "0 8px 32px rgba(15, 23, 32, 0.22)",
		},
	},
	dark: {
		id: "dark",
		colorScheme: "dark",
		colours: dark,
		shadows: {
			shadow1: "0 1px 2px rgba(0, 0, 0, 0.50)",
			shadow2: "0 8px 32px rgba(0, 0, 0, 0.65)",
		},
	},
};

export const highContrastOverrides: Record<"light" | "dark", Partial<RoleColours>> = {
	light: lightHighContrast,
	dark: darkHighContrast,
};

/** The four effective themes: base × contrast. */
export function resolveTheme(base: "light" | "dark", highContrast: boolean): Theme {
	const baseTheme = baseThemes[base];
	if (!highContrast) {
		return baseTheme;
	}
	return {
		id: highContrast ? `${base}-high-contrast` : baseTheme.id,
		colorScheme: baseTheme.colorScheme,
		colours: { ...baseTheme.colours, ...highContrastOverrides[base] },
		shadows: baseTheme.shadows,
	};
}

/**
 * Contrast pairs the CI gate asserts, per effective theme. Thresholds are
 * WCAG 2.x ratios: 4.5 for body text, 3 for large text / non-text UI,
 * 7 for high-contrast themes. `textDisabled` is deliberately absent.
 */
export interface ContrastPair {
	/** Role used as foreground. */
	fg: keyof RoleColours;
	/** Role used as background. */
	bg: keyof RoleColours;
	/** Minimum WCAG ratio in normal themes. */
	min: 3 | 4.5;
	/** Minimum ratio in high-contrast themes (defaults to `min`). */
	minHighContrast?: 3 | 4.5 | 7;
}

export const contrastPairs: readonly ContrastPair[] = [
	{ fg: "text", bg: "bg", min: 4.5, minHighContrast: 7 },
	{ fg: "text", bg: "surface", min: 4.5, minHighContrast: 7 },
	{ fg: "text", bg: "surfaceSunken", min: 4.5, minHighContrast: 7 },
	{ fg: "text", bg: "pagePlaceholder", min: 4.5, minHighContrast: 7 },
	{ fg: "text", bg: "highlightBg", min: 4.5, minHighContrast: 7 },
	{ fg: "text", bg: "highlightActiveBg", min: 4.5, minHighContrast: 7 },
	{ fg: "textSecondary", bg: "bg", min: 4.5, minHighContrast: 7 },
	{ fg: "textSecondary", bg: "surface", min: 4.5, minHighContrast: 7 },
	{ fg: "textMuted", bg: "surface", min: 4.5, minHighContrast: 4.5 },
	{ fg: "textMuted", bg: "bg", min: 4.5, minHighContrast: 4.5 },
	{ fg: "onAccent", bg: "accent", min: 4.5, minHighContrast: 4.5 },
	{ fg: "onAccent", bg: "danger", min: 4.5, minHighContrast: 4.5 },
	{ fg: "link", bg: "surface", min: 4.5, minHighContrast: 7 },
	{ fg: "selectionText", bg: "selectionBg", min: 4.5, minHighContrast: 4.5 },
	{ fg: "highlightText", bg: "highlightBg", min: 4.5, minHighContrast: 7 },
	{ fg: "highlightText", bg: "highlightActiveBg", min: 4.5, minHighContrast: 7 },
	{ fg: "successText", bg: "successSoft", min: 4.5, minHighContrast: 4.5 },
	{ fg: "warningText", bg: "warningSoft", min: 4.5, minHighContrast: 4.5 },
	{ fg: "dangerText", bg: "dangerSoft", min: 4.5, minHighContrast: 4.5 },
	{ fg: "pagePlaceholderText", bg: "pagePlaceholder", min: 4.5, minHighContrast: 7 },
	{ fg: "borderStrong", bg: "surface", min: 3, minHighContrast: 4.5 },
	{ fg: "focusRing", bg: "surface", min: 3, minHighContrast: 4.5 },
];
