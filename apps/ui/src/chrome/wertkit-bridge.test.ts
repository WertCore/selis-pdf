/**
 * SL-4.UI.14 stage 3 - the token bridge cannot silently lose a variable.
 *
 * `@wertkit/ui/styles.css` consumes `--wk-*` names and defines none. The bridge
 * in `@selis/ui-kit` is the only thing standing between a component and an
 * unset custom property (which makes the declaration invalid at
 * computed-value time; the component renders with a browser default rather
 * than failing loudly). This test reads the *installed* stylesheet, so a
 * wertkit upgrade that adds a variable fails here instead of shipping a
 * subtly unthemed control.
 */

import { readFileSync } from "node:fs";
import { createRequire } from "node:module";
import { dirname, join } from "node:path";
import { describe, expect, it } from "vitest";

const require = createRequire(import.meta.url);
const wertkitCss = readFileSync(require.resolve("@wertkit/ui/styles.css"), "utf8");
const bridgePath = require.resolve("@selis/ui-kit/css/wertkit-bridge.css");
const bridgeCss = readFileSync(bridgePath, "utf8");

function withoutComments(css: string): string {
	return css.replace(/\/\*[\s\S]*?\*\//g, "");
}

function declaredValues(css: string): Map<string, Set<string>> {
	const values = new Map<string, Set<string>>();
	for (const match of withoutComments(css).matchAll(/(--wk-[a-z0-9-]+)\s*:\s*([^;]+);/g)) {
		const name = match[1];
		const raw = match[2];
		if (name === undefined || raw === undefined) continue;
		const value = raw.trim().replace(/\s+/g, " ");
		const seen = values.get(name) ?? new Set<string>();
		seen.add(value);
		values.set(name, seen);
	}
	return values;
}

const consumed = [
	...new Set(
		[...wertkitCss.matchAll(/var\(\s*(--wk-[a-z0-9-]+)/g)]
			.map((match) => match[1])
			.filter((name): name is string => name !== undefined),
	),
].sort();
const defined = declaredValues(bridgeCss);

describe("the @wertkit/ui token bridge", () => {
	it("covers every --wk-* variable the installed component CSS consumes", () => {
		expect(consumed).toHaveLength(87);
		const missing = consumed.filter((name) => !defined.has(name));
		expect(missing).toEqual([]);
	});

	it("uses one value per variable across every theme scope", () => {
		const inconsistent = [...defined.entries()]
			.filter(([, values]) => values.size !== 1)
			.map(([name, values]) => `${name}: ${[...values].join(" | ")}`);
		expect(inconsistent).toEqual([]);
	});

	it("does not introduce a raw colour", () => {
		// Structural literals are allowed because they are sizes/line-heights,
		// not theme decisions. Everything colour-bearing must come from a selis
		// token, including the overlay's color-mix().
		const allowedLiterals = new Set(["240px", "480px", "60vh", "auto", "normal"]);
		const rawColour = /#[0-9a-f]{3,8}\b|\b(?:rgb|rgba|hsl|hsla|oklch|lab|lch)\(/i;
		const offenders = [];
		for (const [name, values] of defined) {
			for (const value of values) {
				if (allowedLiterals.has(value)) continue;
				if (rawColour.test(value)) {
					offenders.push(`${name}: ${value}`);
					continue;
				}
				if (!value.includes("var(--selis-") && !value.startsWith("env(safe-area-inset-")) {
					offenders.push(`${name}: ${value}`);
				}
			}
		}
		expect(offenders).toEqual([]);
	});

	it("is exported for hosts and is scoped to the theme elements", () => {
		const pkg = JSON.parse(
			readFileSync(join(dirname(bridgePath), "..", "package.json"), "utf8"),
		) as { exports?: Record<string, string> };
		expect(pkg.exports?.["./css/wertkit-bridge.css"]).toBe("./css/wertkit-bridge.css");
		expect(bridgeCss).toContain(":root,");
		expect(bridgeCss).toContain(".selis-root,");
	});
});
