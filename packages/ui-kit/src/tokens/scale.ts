/**
 * Non-colour tokens (SL-4.10): spacing, radius, typography, elevation z-order,
 * motion, and density. `css/tokens.css` is generated from this module; a unit
 * test keeps the CSS in sync (edit here, then `pnpm --filter @selis/ui-kit
 * test -u`).
 *
 * The spacing scale is a 4 px base grid; every value is a multiple of 2 so the
 * compact density can halve paddings without sub-pixel artefacts.
 */

/** Theme-independent scalar tokens, emitted on `:root` as `--selis-<group>-<name>`. */
export interface ScalarGroup {
	readonly prefix: string;
	/** `name → CSS value` (with units). */
	readonly values: Readonly<Record<string, string>>;
}

export const spacing: ScalarGroup = {
	prefix: "space",
	values: {
		"0": "0px",
		"1": "2px",
		"2": "4px",
		"3": "6px",
		"4": "8px",
		"5": "12px",
		"6": "16px",
		"7": "20px",
		"8": "24px",
		"9": "32px",
		"10": "40px",
		"11": "48px",
		"12": "64px",
	},
};

export const radius: ScalarGroup = {
	prefix: "radius",
	values: {
		sm: "3px",
		md: "6px",
		lg: "10px",
		full: "999px",
	},
};

export const border: ScalarGroup = {
	prefix: "border",
	values: {
		hairline: "1px",
		thick: "2px",
	},
};

export const typography: ScalarGroup = {
	prefix: "text",
	values: {
		xs: "0.75rem",
		sm: "0.8125rem",
		md: "0.875rem",
		lg: "1rem",
		xl: "1.125rem",
		"2xl": "1.375rem",
		"3xl": "1.75rem",
	},
};

export const leading: ScalarGroup = {
	prefix: "leading",
	values: {
		tight: "1.2",
		normal: "1.45",
	},
};

export const weight: ScalarGroup = {
	prefix: "weight",
	values: {
		regular: "400",
		medium: "500",
		semibold: "600",
	},
};

export const fontFamily: ScalarGroup = {
	prefix: "font",
	values: {
		sans: 'system-ui, -apple-system, "Segoe UI", Roboto, "Helvetica Neue", Arial, sans-serif',
		mono: 'ui-monospace, "Cascadia Mono", "SF Mono", Menlo, Consolas, monospace',
	},
};

export const motion: ScalarGroup = {
	prefix: "duration",
	values: {
		fast: "120ms",
		base: "200ms",
		slow: "320ms",
	},
};

export const easing: ScalarGroup = {
	prefix: "ease",
	values: {
		standard: "cubic-bezier(0.2, 0, 0, 1)",
		entrance: "cubic-bezier(0, 0, 0.2, 1)",
		exit: "cubic-bezier(0.4, 0, 1, 1)",
	},
};

export const zIndex: ScalarGroup = {
	prefix: "z",
	values: {
		base: "0",
		toolbar: "10",
		panel: "20",
		"dialog-backdrop": "40",
		dialog: "50",
		toast: "60",
	},
};

/** Density variants: emitted as `[data-density="…"]` override blocks. */
export const densityVariants: Readonly<
	Record<"comfortable" | "compact", Readonly<Record<string, string>>>
> = {
	comfortable: {
		"control-height": "32px",
		"control-gap": "8px",
		"control-pad-x": "12px",
		"toolbar-pad-block": "6px",
		"toolbar-pad-inline": "8px",
		"panel-header-height": "40px",
		"row-height": "32px",
		"page-gap": "16px",
	},
	compact: {
		"control-height": "28px",
		"control-gap": "6px",
		"control-pad-x": "8px",
		"toolbar-pad-block": "4px",
		"toolbar-pad-inline": "6px",
		"panel-header-height": "34px",
		"row-height": "26px",
		"page-gap": "12px",
	},
};

/** Groups emitted once on `:root` (theme-independent). */
export const scalarGroups: readonly ScalarGroup[] = [
	spacing,
	radius,
	border,
	typography,
	leading,
	weight,
	fontFamily,
	motion,
	easing,
	zIndex,
];
