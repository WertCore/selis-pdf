/**
 * Colour tokens — the single source of truth for every colour the UI uses
 * (SL-4.UI.10, vocabulary per SL-4.UI.14). Values are sRGB hex so WCAG contrast
 * is computable in tests; `css/tokens.css` is generated from this data and a
 * unit test keeps the two in sync (run `pnpm --filter @selis/ui-kit test -u`
 * after editing here).
 *
 * The role *names* follow wertkit's naming contract
 * (https://github.com/WertCore/wertkit/blob/main/spec/naming.md) so Selis and
 * wertkit speak one vocabulary. We keep our own token files rather than
 * depending on `@wertkit/tokens`: wertkit has no `data-contrast` high-contrast
 * theme and UI.10's tests assert 7:1 in one.
 *
 * Rules:
 * - Every value is bound to a *role*, never used raw. App code references
 *   `var(--selis-color-<role>)` only; raw palette values never leave this file.
 * - `fg-disabled` is the only role exempt from the 4.5:1 contrast gate
 *   (WCAG 1.4.1 disabled-control carve-out); do not use it for anything
 *   the user must read.
 * - High-contrast themes override roles; they must keep every pair that the
 *   base theme passes at its (higher) HC threshold.
 */

export type Hex = string;

/**
 * Every colour role the design system defines. All are required per theme.
 *
 * Names follow wertkit's contract. Roles wertkit does not define are marked
 * "Selis extension" — they keep the same shape (`<role>` / `<role>-fg` /
 * `<role>-subtle`) so a future wertkit role drops in without a rename, but they
 * are not part of the shared vocabulary.
 */
export interface RoleColours {
	/** App background behind everything (the area around pages). */
	bg: Hex;
	/** Panels, toolbars, dialogs, cards — surfaces lifted off `bg`. */
	bgRaised: Hex;
	/** Wells and recessed areas inside raised surfaces. */
	bgInset: Hex;
	/** Primary text on `bg`, `bgRaised`, `bgInset`. */
	fg: Hex;
	/** Secondary text: labels, metadata, inactive list items. */
	fgMuted: Hex;
	/** Tertiary text: hints, captions, page labels. Still ≥ 4.5:1. */
	fgSubtle: Hex;
	/** Disabled text. Selis extension. Exempt from the contrast gate (WCAG 1.4.1). */
	fgDisabled: Hex;
	/** Hairline borders, dividers. Non-text usage. */
	border: Hex;
	/** Emphasised borders: input focus frames, dividers on busy art. */
	borderStrong: Hex;
	/** Keyboard focus indicator. Must clear 3:1 (HC: 4.5:1) on `bgRaised`. */
	focusRing: Hex;
	/** Primary action background; `accentFg` text sits on it. */
	accent: Hex;
	/** Hover/pressed state for `accent`. */
	accentHover: Hex;
	/** Quiet accent tint (selected nav items, toggles off-state). */
	accentSubtle: Hex;
	/** Text/icons on `accent`, `accentHover`, and solid status fills. */
	accentFg: Hex;
	/** Link text. Selis extension. ≥ 4.5:1 on `bgRaised`. */
	link: Hex;
	/** Text-selection background (also used by the text layer). Selis extension. */
	selectionBg: Hex;
	/** Text colour over `selectionBg`. Selis extension. */
	selectionFg: Hex;
	/** Search-match highlight background under the text layer. Selis extension. */
	highlightBg: Hex;
	/** Current-match highlight background. Selis extension. */
	highlightActiveBg: Hex;
	/** Text colour over both highlight backgrounds. Selis extension. */
	highlightFg: Hex;
	/** Solid success accent (progress, badges use the soft pair). */
	success: Hex;
	/** Success badge background; pair with `successFg`. */
	successSubtle: Hex;
	/** Success text on `successSubtle`. */
	successFg: Hex;
	/** Solid warning accent. wertkit spells this role `warn`. */
	warn: Hex;
	/** Warning badge background; pair with `warnFg`. */
	warnSubtle: Hex;
	/** Warning text on `warnSubtle`. */
	warnFg: Hex;
	/** Solid danger accent (destructive buttons; `accentFg` text on it). */
	danger: Hex;
	/** Danger badge background; pair with `dangerFg`. */
	dangerSubtle: Hex;
	/** Danger text on `dangerSubtle`. */
	dangerFg: Hex;
	/** Page-tile placeholder while a render resolves. Selis extension. */
	pagePlaceholder: Hex;
	/** Page label text on `pagePlaceholder`. Selis extension. */
	pagePlaceholderFg: Hex;
	/** Scrollbar thumb (track uses `bgInset`). Selis extension. */
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
	bgRaised: "#ffffff",
	bgInset: "#e4e8ec",
	fg: "#171b1f",
	fgMuted: "#47525c",
	fgSubtle: "#5d6975",
	fgDisabled: "#9aa4ad",
	border: "#d3d9df",
	borderStrong: "#8a949e",
	focusRing: "#2557d0",
	accent: "#2b5bc4",
	accentHover: "#234aa8",
	accentSubtle: "#dbe5fb",
	accentFg: "#ffffff",
	link: "#1d4fc0",
	selectionBg: "#2b5bc4",
	selectionFg: "#ffffff",
	highlightBg: "#ffe066",
	highlightActiveBg: "#ffab40",
	highlightFg: "#171b1f",
	success: "#1e7d43",
	successSubtle: "#dcf2e3",
	successFg: "#1a6b38",
	warn: "#7a4d00",
	warnSubtle: "#fdeec8",
	warnFg: "#7a4d00",
	danger: "#b3261e",
	dangerSubtle: "#fbe2e0",
	dangerFg: "#a12118",
	pagePlaceholder: "#dfe3e8",
	pagePlaceholderFg: "#47525c",
	scrollbarThumb: "#b8bfc7",
	shadow: "#0f1720",
};

const dark: RoleColours = {
	bg: "#121519",
	bgRaised: "#1d2229",
	bgInset: "#0d1014",
	fg: "#e9edf1",
	fgMuted: "#b6c0c9",
	fgSubtle: "#8f9aa4",
	fgDisabled: "#5c6670",
	border: "#333b44",
	borderStrong: "#7d8892",
	focusRing: "#8fb4f5",
	accent: "#3667dc",
	accentHover: "#4a78ea",
	accentSubtle: "#223252",
	accentFg: "#ffffff",
	link: "#9cbdf3",
	selectionBg: "#3667dc",
	selectionFg: "#ffffff",
	highlightBg: "#7a5c00",
	highlightActiveBg: "#965700",
	highlightFg: "#e9edf1",
	success: "#35a05f",
	successSubtle: "#1d3a28",
	successFg: "#63c788",
	warn: "#ecc069",
	warnSubtle: "#413410",
	warnFg: "#ecc069",
	danger: "#c33c30",
	dangerSubtle: "#44201d",
	dangerFg: "#f28f85",
	pagePlaceholder: "#232a32",
	pagePlaceholderFg: "#b6c0c9",
	scrollbarThumb: "#454f59",
	shadow: "#000000",
};

/** High-contrast overrides on top of the light theme. */
export const lightHighContrast: Partial<RoleColours> = {
	bg: "#fafafa",
	bgRaised: "#ffffff",
	bgInset: "#f0f0f0",
	fg: "#000000",
	fgMuted: "#1a1d21",
	fgSubtle: "#333a41",
	border: "#6d7780",
	borderStrong: "#000000",
	focusRing: "#00339e",
	accent: "#123a9e",
	accentHover: "#0e2f82",
	accentSubtle: "#dce6ff",
	link: "#00339e",
	selectionBg: "#00339e",
	highlightBg: "#ffd23f",
	highlightActiveBg: "#ff9e1f",
	success: "#0d5c2e",
	successSubtle: "#d6f0dd",
	successFg: "#0d5c2e",
	warn: "#5f3d00",
	warnSubtle: "#ffefc2",
	warnFg: "#5f3d00",
	danger: "#a81b12",
	dangerSubtle: "#ffd9d5",
	dangerFg: "#8f1d15",
	pagePlaceholder: "#eceef1",
	pagePlaceholderFg: "#1a1d21",
	scrollbarThumb: "#6d7780",
};

/** High-contrast overrides on top of the dark theme. */
export const darkHighContrast: Partial<RoleColours> = {
	bg: "#000000",
	bgRaised: "#0d1014",
	bgInset: "#000000",
	fg: "#ffffff",
	fgMuted: "#dbe3ea",
	fgSubtle: "#c3ccd4",
	fgDisabled: "#6d7780",
	border: "#8a949e",
	borderStrong: "#ffffff",
	focusRing: "#bfe3ff",
	accent: "#2451c8",
	accentHover: "#3763dd",
	accentSubtle: "#12233f",
	link: "#cfe2ff",
	selectionBg: "#2451c8",
	highlightBg: "#6e5200",
	highlightActiveBg: "#7a4300",
	highlightFg: "#ffffff",
	success: "#4cc27a",
	successSubtle: "#14301f",
	successFg: "#7fe0a2",
	warn: "#ffd07a",
	warnSubtle: "#3a2c08",
	warnFg: "#ffd07a",
	danger: "#c9372b",
	dangerSubtle: "#431815",
	dangerFg: "#ffb0a6",
	pagePlaceholder: "#1d2229",
	pagePlaceholderFg: "#ffffff",
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
 * 7 for high-contrast themes. `fgDisabled` is deliberately absent.
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
	{ fg: "fg", bg: "bg", min: 4.5, minHighContrast: 7 },
	{ fg: "fg", bg: "bgRaised", min: 4.5, minHighContrast: 7 },
	{ fg: "fg", bg: "bgInset", min: 4.5, minHighContrast: 7 },
	{ fg: "fg", bg: "pagePlaceholder", min: 4.5, minHighContrast: 7 },
	{ fg: "fg", bg: "highlightBg", min: 4.5, minHighContrast: 7 },
	{ fg: "fg", bg: "highlightActiveBg", min: 4.5, minHighContrast: 7 },
	{ fg: "fgMuted", bg: "bg", min: 4.5, minHighContrast: 7 },
	{ fg: "fgMuted", bg: "bgRaised", min: 4.5, minHighContrast: 7 },
	{ fg: "fgSubtle", bg: "bgRaised", min: 4.5, minHighContrast: 4.5 },
	{ fg: "fgSubtle", bg: "bg", min: 4.5, minHighContrast: 4.5 },
	{ fg: "accentFg", bg: "accent", min: 4.5, minHighContrast: 4.5 },
	{ fg: "accentFg", bg: "danger", min: 4.5, minHighContrast: 4.5 },
	{ fg: "link", bg: "bgRaised", min: 4.5, minHighContrast: 7 },
	{ fg: "selectionFg", bg: "selectionBg", min: 4.5, minHighContrast: 4.5 },
	{ fg: "highlightFg", bg: "highlightBg", min: 4.5, minHighContrast: 7 },
	{ fg: "highlightFg", bg: "highlightActiveBg", min: 4.5, minHighContrast: 7 },
	{ fg: "successFg", bg: "successSubtle", min: 4.5, minHighContrast: 4.5 },
	{ fg: "warnFg", bg: "warnSubtle", min: 4.5, minHighContrast: 4.5 },
	{ fg: "dangerFg", bg: "dangerSubtle", min: 4.5, minHighContrast: 4.5 },
	{ fg: "pagePlaceholderFg", bg: "pagePlaceholder", min: 4.5, minHighContrast: 7 },
	{ fg: "borderStrong", bg: "bgRaised", min: 3, minHighContrast: 4.5 },
	{ fg: "focusRing", bg: "bgRaised", min: 3, minHighContrast: 4.5 },
];
