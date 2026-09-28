/**
 * The CSS discipline ui-kit applies to `base.css`, applied to UI.06's component
 * stylesheet. It is `page-list.css.test.ts` pointed at `navigation.css`, and it
 * exists because the same four rules erode otherwise:
 *
 * 1. every class is under the `selis-` namespace;
 * 2. every `var(--selis-…)` names a variable the **generated** token CSS defines
 *    (checked against `generateTokensCss()`, not the committed file);
 * 3. no raw colour literal and no length a token defines;
 * 4. every class a controller can emit is actually styled here or in ui-kit's
 *    `base.css` — so the outline cannot ship with a modifier nobody styled.
 *
 * Rule 4 is the one that needs the *sources*: the class names are built in
 * TypeScript, and a stylesheet that forgets one produces an unstyled row rather
 * than an error. Both controllers are scanned, so the check is structural
 * instead of a list somebody has to remember to update.
 */

import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { generateTokensCss } from "@selis/ui-kit";
import { describe, expect, it } from "vitest";

const here = dirname(fileURLToPath(import.meta.url));
const appCss = readFileSync(join(here, "navigation.css"), "utf8");
const baseCss = readFileSync(
	join(here, "..", "..", "..", "..", "packages", "ui-kit", "css", "base.css"),
	"utf8",
);
/** The sources that build class names, so rule 4 scans rather than lists. */
const controllerSources = ["navigation.ts", "thumbnails.ts"].map((name) =>
	readFileSync(join(here, name), "utf8"),
);

/** Comments carry prose about tokens; only the rules are checked. */
const rulesOnly = (css: string): string => css.replace(/\/\*[\s\S]*?\*\//g, "");

const definedTokens = (): Set<string> =>
	new Set(
		[...generateTokensCss().matchAll(/(--selis-[a-z0-9-]+):/g)].flatMap((match) =>
			match[1] !== undefined ? [match[1]] : [],
		),
	);

describe("navigation.css", () => {
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
		expect(css).not.toMatch(/#[0-9a-fA-F]{3,8}\b/);
		expect(css).not.toMatch(/\b(?:rgb|rgba|hsl|hsla)\(/);
		// A `var(…, fallback)` carries a literal too, so the var() calls are
		// removed first: what remains may contain only keywords, percentages and
		// bare 0/1. A raw `12px` here is a token that was bypassed.
		const declarations = [...css.matchAll(/:\s*([^;{}]+);/g)].flatMap((match) =>
			match[1] !== undefined ? [match[1].replace(/var\([^)]*\)/g, "").trim()] : [],
		);
		const offenders = declarations.filter((value) =>
			/\d+(?:\.\d+)?(?:px|rem|em|ms)\b/.test(value),
		);
		expect(offenders, `literal dimensions: ${offenders.join(" | ")}`).toEqual([]);
	});

	it("styles every class the controllers emit", () => {
		const emitted = new Set<string>();
		for (const source of controllerSources) {
			for (const match of source.matchAll(/"(selis-[a-z-]*)"/g)) {
				if (match[1] !== undefined) {
					emitted.add(match[1]);
				}
			}
		}
		expect(emitted.size).toBeGreaterThan(0);
		const styled = new Set(
			[...`${appCss}\n${baseCss}`.matchAll(/\.(selis-[a-zA-Z0-9_-]+)/g)].flatMap((match) =>
				match[1] !== undefined ? [match[1]] : [],
			),
		);
		const unstyled = [...emitted].filter((name) => !styled.has(name));
		expect(unstyled, `class names with no styles: ${unstyled.join(", ")}`).toEqual([]);
	});

describe("navigation.css, continued", () => {
	it("keeps geometry carriers out of the token namespace", () => {
		// `--thumb-w` and `--row-indent` are document data; `--selis-*` is design.
		// Mixing the two is how a computed pixel value becomes a design decision
		// by accident, and `page-list.css` makes the same rule for `--tile-x`.
		const carriers = [...appCss.matchAll(/var\(\s*(--[a-z0-9-]+)/g)].flatMap((match) =>
			match[1] !== undefined ? [match[1]] : [],
		);
		const stray = [...new Set(carriers)].filter((name) => !name.startsWith("--selis-")).sort();
		expect(stray, `carriers outside the namespace: ${stray.join(", ")}`).toEqual([
			"--row-indent",
			"--thumb-h",
			"--thumb-w",
		]);
	});

	it("gives every focusable surface a visible focus ring", () => {
		// The outline and the rail are the two keyboard-reachable surfaces this
		// file owns. A stylesheet that styles `:hover` and forgets
		// `:focus-visible` is the most common way a viewer loses its keyboard.
		const focusRules = [
			...rulesOnly(appCss).matchAll(/:focus-visible\s*\{([^}]*)\}/g),
		].flatMap((match) => (match[1] !== undefined ? [match[1]] : []));
		expect(focusRules.length).toBeGreaterThanOrEqual(2);
		for (const rule of focusRules) {
			expect(rule, "a focus ring").toContain("outline");
		}
	});

	it("keeps the thumbnail box out of the flow of its own bitmap", () => {
		// `position: relative` plus a fixed box is what makes the label a caption
		// rather than a second line, and what keeps a resolving preview from
		// changing the rail's height.
		const thumb = /\.selis-thumbnail\s*\{([^}]*)\}/.exec(rulesOnly(appCss))?.[1] ?? "";
		expect(thumb).toContain("position: relative");
		expect(thumb).toContain("contain");
		// The var() fallbacks are stripped first, exactly as the tokenised-length
		// check does: `var(--thumb-w, 0px)` is a carrier with a default, not a
		// hard-coded size.
		expect(thumb.replace(/var\([^)]*\)/g, "")).not.toMatch(/\d+(?:\.\d+)?(?:px|rem|em)\b/);
	});

	it("gives the outline rows a roving-focus hook that does not move them", () => {
		// `:focus-visible` rather than `:focus`, and `outline` rather than
		// `border`: a keyboard reader must be able to see where they are, and
		// showing it must not reflow the sidebar.
		const css = rulesOnly(appCss);
		expect(css).toContain(".selis-outline__row:focus-visible");
		expect(css).not.toContain(".selis-outline__row:focus {");
	});
});

});
