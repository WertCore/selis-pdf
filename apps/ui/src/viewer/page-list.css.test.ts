/**
 * The CSS discipline ui-kit applies to its own `base.css`, applied to the app's
 * component stylesheet (SL-4.UI.02). `@selis/ui-kit`'s README makes this the
 * rule for UI.02+ components, and a rule that is only written down is a rule
 * that erodes:
 *
 * 1. every class is under the `selis-` namespace;
 * 2. every `var(--selis-…)` reference names a variable the **generated** token
 *    CSS actually defines (checked against `generateTokensCss()`, not against
 *    the committed file, so a token that is only in the source does not pass);
 * 3. no raw colour literal and no length/typography literal a token defines;
 * 4. every class name the controller emits in `PageTileView.className` is
 *    actually styled by this file or by ui-kit's `base.css` — so a tile cannot
 *    ship with a modifier the stylesheet forgot.
 */

import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { generateTokensCss } from "@selis/ui-kit";
import { describe, expect, it } from "vitest";

const here = dirname(fileURLToPath(import.meta.url));
const appCss = readFileSync(join(here, "page-list.css"), "utf8");
const baseCss = readFileSync(
	join(here, "..", "..", "..", "..", "packages", "ui-kit", "css", "base.css"),
	"utf8",
);
/** The controller source, for the class names it actually emits. */
const controllerSource = readFileSync(join(here, "page-list.ts"), "utf8");

/** Comments carry prose about tokens; only the rules are checked. */
const rulesOnly = (css: string): string => css.replace(/\/\*[\s\S]*?\*\//g, "");

const definedTokens = (): Set<string> =>
	new Set(
		[...generateTokensCss().matchAll(/(--selis-[a-z0-9-]+):/g)].flatMap((match) =>
			match[1] !== undefined ? [match[1]] : [],
		),
	);

describe("page-list.css", () => {
	it("keeps every class under the selis- namespace", () => {
		const classes = [...rulesOnly(appCss).matchAll(/\.([a-zA-Z][a-zA-Z0-9_-]*)/g)].flatMap(
			(match) => (match[1] !== undefined ? [match[1]] : []),
		);
		const foreign = classes.filter((name) => !name.startsWith("selis-"));
		expect(foreign, `non-namespaced classes: ${foreign.join(", ")}`).toEqual([]);
		expect(classes.length).toBeGreaterThan(3);
	});

	it("references only tokens the generator defines", () => {
		const defined = definedTokens();
		const used = [...appCss.matchAll(/var\(\s*(--selis-[a-z0-9-]+)/g)].flatMap((match) =>
			match[1] !== undefined ? [match[1]] : [],
		);
		const unknown = [...new Set(used)].filter((name) => !defined.has(name));
		expect(unknown, `unknown token references: ${unknown.join(", ")}`).toEqual([]);
		expect(used.length).toBeGreaterThan(3);
	});

	it("hard-codes no colour and no tokenised length", () => {
		const css = rulesOnly(appCss);
		// Colours come from roles, never from literals.
		expect(css).not.toMatch(/#[0-9a-fA-F]{3,8}\b/);
		expect(css).not.toMatch(/\b(?:rgb|rgba|hsl|hsla)\(/);
		// Lengths and typography come from tokens. A `var(…, fallback)` carries a
		// literal too, so the var() calls are removed first: what remains must
		// contain no absolute dimension, only keywords, percentages (relative by
		// definition, so never a token) and bare 0/1. A raw `12px` here is a
		// token that was bypassed.
		const declarations = [...css.matchAll(/:\s*([^;{}]+);/g)].flatMap((match) =>
			match[1] !== undefined ? [match[1].replace(/var\([^)]*\)/g, "").trim()] : [],
		);
		const offenders = declarations.filter((value) => /\d+(?:\.\d+)?(?:px|rem|em|ms)\b/.test(value));
		expect(offenders, `literal dimensions: ${offenders.join(" | ")}`).toEqual([]);
	});

	it("styles every class the controller emits", () => {
		const emitted = [...controllerSource.matchAll(/"(selis-page-tile[a-z-]*)"/g)].flatMap(
			(match) => (match[1] !== undefined ? [match[1]] : []),
		);
		expect(emitted.length).toBeGreaterThan(3);
		const styled = new Set(
			[...`${appCss}\n${baseCss}`.matchAll(/\.(selis-[a-zA-Z0-9_-]+)/g)].flatMap((match) =>
				match[1] !== undefined ? [match[1]] : [],
			),
		);
		const unstyled = [...new Set(emitted)].filter((name) => !styled.has(name));
		expect(unstyled, `class names with no styles: ${unstyled.join(", ")}`).toEqual([]);
	});

	it("positions placed tiles absolutely, which is what makes the rungs share a box", () => {
		const css = rulesOnly(appCss);
		const placed = /\.selis-page-tile--placed\s*\{([^}]*)\}/.exec(css)?.[1] ?? "";
		expect(placed).toContain("position: absolute");
		// Geometry comes from custom properties the shell sets per tile, not from
		// a hard-coded box — and those carriers stay out of the token namespace.
		for (const carrier of ["--tile-x", "--tile-y", "--tile-w", "--tile-h"]) {
			expect(placed, carrier).toContain(`var(${carrier}`);
		}
		expect(rulesOnly(appCss)).not.toContain("--selis-tile-");
	});
});
