import { readFileSync, writeFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { describe, expect, it } from "vitest";
import {
	type RoleColours,
	VERSION,
	baseThemes,
	contrastPairs,
	contrastRatio,
	densityVariants,
	generateTokensCss,
	highContrastOverrides,
	radius,
	resolveTheme,
	scalarGroups,
	spacing,
	typography,
} from "./index.js";

const here = dirname(fileURLToPath(import.meta.url));
const tokensCssPath = join(here, "../css/tokens.css");
const baseCssPath = join(here, "../css/base.css");

const referenceRoles = Object.keys(baseThemes.light.colours) as Array<keyof RoleColours & string>;

describe("token integrity", () => {
	it("every effective theme defines exactly the reference role set", () => {
		for (const base of ["light", "dark"] as const) {
			for (const highContrast of [false, true]) {
				const theme = resolveTheme(base, highContrast);
				const keys = Object.keys(theme.colours);
				expect(new Set(keys), theme.id).toEqual(new Set(referenceRoles));
				for (const role of referenceRoles) {
					expect(theme.colours[role], `${theme.id}.${role}`).toMatch(/^#[0-9a-fA-F]{6}$/);
				}
			}
		}
	});

	it("high-contrast overrides only touch known roles", () => {
		for (const base of ["light", "dark"] as const) {
			for (const role of Object.keys(highContrastOverrides[base])) {
				expect(referenceRoles).toContain(role);
			}
		}
	});

	it("roles that must stay visually distinct are distinct", () => {
		for (const base of ["light", "dark"] as const) {
			for (const highContrast of [false, true]) {
				const c = resolveTheme(base, highContrast).colours;
				const id = resolveTheme(base, highContrast).id;
				expect(new Set([c.text, c.textSecondary, c.textMuted]).size, `${id}: text ramp`).toBe(3);
				expect(c.borderSubtle === c.borderStrong, `${id}: borders`).toBe(false);
				expect(c.bg === c.surface, `${id}: bg vs surface`).toBe(false);
				expect(c.pagePlaceholder === c.bg, `${id}: placeholder must be visible on bg`).toBe(false);
				expect(c.highlightBg === c.surface, `${id}: highlight must be visible on surface`).toBe(
					false,
				);
			}
		}
	});

	it("contrast pairs demand at least as much in high-contrast themes", () => {
		for (const pair of contrastPairs) {
			expect(pair.minHighContrast ?? pair.min).toBeGreaterThanOrEqual(pair.min);
		}
	});
});

describe("contrast gate (WCAG 2.x)", () => {
	for (const base of ["light", "dark"] as const) {
		for (const highContrast of [false, true]) {
			const theme = resolveTheme(base, highContrast);
			it(`${theme.id} passes every declared pair`, () => {
				for (const pair of contrastPairs) {
					const min = highContrast ? (pair.minHighContrast ?? pair.min) : pair.min;
					const ratio = contrastRatio(theme.colours[pair.fg], theme.colours[pair.bg]);
					expect(
						ratio,
						`${theme.id}: ${pair.fg} on ${pair.bg} (${theme.colours[pair.fg]} on ${theme.colours[pair.bg]})`,
					).toBeGreaterThanOrEqual(min);
				}
			});
		}
	}
});

describe("scalar scales", () => {
	it("values carry sane units", () => {
		const unitful = /^(\d+(?:\.\d+)?)(px|rem|ms)$/;
		for (const group of scalarGroups) {
			for (const [name, value] of Object.entries(group.values)) {
				if (["leading", "weight", "z"].includes(group.prefix)) {
					expect(value, `${group.prefix}-${name}`).toMatch(/^\d+(?:\.\d+)?$/);
				} else if (["font", "ease"].includes(group.prefix)) {
					expect(value.length, `${group.prefix}-${name}`).toBeGreaterThan(0);
				} else {
					expect(value, `${group.prefix}-${name}`).toMatch(unitful);
				}
			}
		}
	});

	it("spacing sits on the 2px grid", () => {
		for (const value of Object.values(spacing.values)) {
			const px = Number.parseInt(value, 10);
			expect(px % 2).toBe(0);
		}
	});

	it("type scale is monotonic", () => {
		let prev: number | undefined;
		for (const value of Object.values(typography.values)) {
			const size = Number.parseFloat(value);
			if (prev !== undefined) {
				expect(size).toBeGreaterThan(prev);
			}
			prev = size;
		}
	});

	it("density variants share keys and compact is never roomier", () => {
		const comfortable = Object.keys(densityVariants.comfortable).sort();
		const compact = Object.keys(densityVariants.compact).sort();
		expect(compact).toEqual(comfortable);
		for (const key of comfortable) {
			const compactPx = Number.parseInt(densityVariants.compact[key] ?? "", 10);
			const comfortablePx = Number.parseInt(densityVariants.comfortable[key] ?? "", 10);
			expect(Number.isNaN(compactPx), key).toBe(false);
			expect(compactPx, key).toBeLessThanOrEqual(comfortablePx);
		}
	});

	it("radius stays tokenised (no arbitrary values in components)", () => {
		expect(Object.keys(radius.values).length).toBeGreaterThanOrEqual(3);
	});
});

describe("shipped CSS", () => {
	it("css/tokens.css matches the generator (UPDATE_TOKENS_CSS=1 rewrites it)", () => {
		const generated = generateTokensCss();
		if (process.env.UPDATE_TOKENS_CSS === "1") {
			writeFileSync(tokensCssPath, generated, "utf8");
		}
		expect(readFileSync(tokensCssPath, "utf8")).toBe(generated);
	});

	it("base.css only references variables the generator defines", () => {
		const generated = generateTokensCss();
		const defined = new Set(
			[...generated.matchAll(/^(?:\t)?(--selis-[a-z0-9-]+):/gm)].flatMap((m) =>
				m[1] !== undefined ? [m[1]] : [],
			),
		);
		expect(defined.size).toBeGreaterThan(40);
		const used = [
			...readFileSync(baseCssPath, "utf8").matchAll(/var\(\s*(--selis-[a-z0-9-]+)/g),
		].flatMap((m) => (m[1] !== undefined ? [m[1]] : []));
		const unknown = used.filter((name) => !defined.has(name));
		expect(unknown, `unknown token references: ${unknown.join(", ")}`).toEqual([]);
	});

	it("base.css classes stay under the selis- namespace", () => {
		const css = readFileSync(baseCssPath, "utf8").replace(/\/\*[\s\S]*?\*\//g, "");
		const classTokens = [...css.matchAll(/\.([a-zA-Z][a-zA-Z0-9_-]*)/g)].flatMap((m) =>
			m[1] !== undefined ? [m[1]] : [],
		);
		const foreign = classTokens.filter((name) => !name.startsWith("selis-"));
		expect(foreign, `non-namespaced classes: ${foreign.join(", ")}`).toEqual([]);
	});
});

describe("package", () => {
	it("VERSION matches package.json", () => {
		const pkg: { version: string } = JSON.parse(
			readFileSync(join(here, "../package.json"), "utf8"),
		);
		expect(VERSION).toBe(pkg.version);
	});

	it("exposes the CSS subpaths the README tells consumers to import", () => {
		const pkg: { exports: Record<string, unknown> } = JSON.parse(
			readFileSync(join(here, "../package.json"), "utf8"),
		);
		for (const subpath of ["./css/tokens.css", "./css/base.css"]) {
			expect(pkg.exports[subpath], subpath).toBeDefined();
		}
	});
});
