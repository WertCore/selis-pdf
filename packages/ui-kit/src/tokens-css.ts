/**
 * CSS generator for the design tokens. `css/tokens.css` is committed and a
 * unit test asserts it matches this function's output byte-for-byte, so the
 * TS data and the shipped CSS cannot drift. Update flow:
 *
 *   1. edit `src/tokens/*`
 *   2. `UPDATE_TOKENS_CSS=1 pnpm --filter @selis/ui-kit test`  (or `vitest -u`)
 *   3. review the diff in `css/tokens.css`
 */

import {
	type RoleColours,
	type Theme,
	baseThemes,
	contrastPairs,
	highContrastOverrides,
	resolveTheme,
} from "./tokens/colour.js";
import { contrastRatio } from "./tokens/contrast.js";
import { densityVariants, scalarGroups } from "./tokens/scale.js";

export const TOKENS_CSS_HEADER = [
	"/* GENERATED FILE — do not edit by hand.",
	" * Source of truth: packages/ui-kit/src/tokens/*.ts",
	" * Update: UPDATE_TOKENS_CSS=1 pnpm --filter @selis/ui-kit test",
	" * Part of SL-4.UI.10 (design system + theming). */",
	"",
].join("\n");

/** camelCase / PascalCase role name → kebab-case CSS name fragment. */
function kebab(name: string): string {
	return name.replace(/([a-z0-9])([A-Z])/g, "$1-$2").toLowerCase();
}

function varName(role: keyof RoleColours & string): string {
	return `--selis-color-${kebab(role)}`;
}

/** Block of custom properties for one theme's colour roles. */
function colourBlock(theme: Theme): string[] {
	const lines: string[] = [`color-scheme: ${theme.colorScheme};`];
	for (const role of Object.keys(theme.colours) as Array<keyof RoleColours & string>) {
		lines.push(`${varName(role)}: ${theme.colours[role]};`);
	}
	lines.push(`--selis-shadow-1: ${theme.shadows.shadow1};`);
	lines.push(`--selis-shadow-2: ${theme.shadows.shadow2};`);
	return lines;
}

/** Emit one CSS rule: `[indent]selector {` + indented declarations + `}`. */
function rule(selector: string, declarations: readonly string[], indent = ""): string {
	return [
		`${indent}${selector} {`,
		...declarations.map((d) => `${indent}\t${d}`),
		`${indent}}`,
	].join("\n");
}

/** Blocked/paired contrast roles the generator double-checks while emitting. */
function assertGateContrast(): void {
	const failures: string[] = [];
	for (const base of ["light", "dark"] as const) {
		for (const highContrast of [false, true]) {
			const theme = resolveTheme(base, highContrast);
			for (const pair of contrastPairs) {
				const min = highContrast ? (pair.minHighContrast ?? pair.min) : pair.min;
				const fg = theme.colours[pair.fg];
				const bg = theme.colours[pair.bg];
				const ratio = contrastRatio(fg, bg);
				if (ratio < min) {
					failures.push(
						`${theme.id}: ${pair.fg} on ${pair.bg} is ${ratio.toFixed(2)}:1, needs ${min}:1 (${fg} on ${bg})`,
					);
				}
			}
		}
	}
	if (failures.length > 0) {
		throw new Error(`contrast gate failed:\n${failures.join("\n")}`);
	}
}

/** Generate the full `tokens.css` content. Throws if the contrast gate fails. */
export function generateTokensCss(): string {
	assertGateContrast();

	const parts: string[] = [TOKENS_CSS_HEADER];

	// `:root` — light theme colours + all theme-independent scalars.
	const root: string[] = colourBlock(baseThemes.light);
	for (const group of scalarGroups) {
		for (const [name, value] of Object.entries(group.values)) {
			root.push(`--selis-${group.prefix}-${name}: ${value};`);
		}
	}
	for (const [name, value] of Object.entries(densityVariants.comfortable)) {
		root.push(`--selis-density-${name}: ${value};`);
	}
	parts.push(rule(":root", root));

	// Dark theme.
	parts.push(rule('[data-theme="dark"]', colourBlock(baseThemes.dark)));

	// High-contrast overrides: only the overridden roles are re-emitted.
	for (const base of ["light", "dark"] as const) {
		const selector =
			base === "light"
				? `[data-theme="light"][data-contrast="high"]`
				: `[data-theme="dark"][data-contrast="high"]`;
		const decls = (
			Object.keys(highContrastOverrides[base]) as Array<keyof RoleColours & string>
		).map((role) => `${varName(role)}: ${highContrastOverrides[base][role]};`);
		parts.push(rule(selector, decls));
	}

	// Density overrides (comfortable lives in `:root`).
	parts.push(
		rule(
			'[data-density="compact"]',
			Object.entries(densityVariants.compact).map(
				([name, value]) => `--selis-density-${name}: ${value};`,
			),
		),
	);

	// Reduced motion: both the media query and the manual override collapse
	// every duration to zero. Components use the duration tokens, so they all
	// stop; the placeholder shimmer also checks `prefers-reduced-motion`.
	const motionOff = [
		"--selis-duration-fast: 0ms;",
		"--selis-duration-base: 0ms;",
		"--selis-duration-slow: 0ms;",
	];
	parts.push(rule("@media (prefers-reduced-motion: reduce)", [rule(":root", motionOff, "\t")]));
	parts.push(rule('[data-motion="reduced"]', motionOff));

	return `${parts.join("\n\n")}\n`;
}
