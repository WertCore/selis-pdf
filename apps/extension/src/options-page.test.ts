/**
 * SL-4.EXT.07 — the shipped options page, the manifest that names it, and the
 * stylesheet it links.
 *
 * This is the same kind of test as `manifest.test.ts`: it reads the **files the
 * store upload will contain** rather than the TypeScript that produced them,
 * because every claim this page makes is a claim about bytes.
 *
 * What is asserted, and why each one is a failure someone would want:
 *
 * | Assertion | The failure it prevents |
 * |---|---|
 * | `options_page` is declared and names a shipped file | the manifest points at a page the packer never copied — a 404 with every gate green, the EXT.03 failure mode |
 * | the page has no text outside `data-i18n` keys | an English literal in markup: untranslatable, and invisible to the catalogue test |
 * | every key the markup asks for exists | a blank line in front of a user |
 * | no `chrome.tabs`, no network, no `http` in the page or the worker | a "helpful" link quietly widening what this extension may do |
 * | no colour literal in the stylesheet, and every token it names is real | a hard-coded colour that ignores the theme, or a renamed role resolving to nothing |
 * | the stylesheet keeps `[hidden]` working | the first-run flag controlling nothing, because the guide is always visible |
 * | the page adds no permission | a quiet scope creep past the EXT.01 review |
 */

import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { describe, expect, it } from "vitest";
import { findHtmlViolations, findManifestViolations } from "./bundle-docs.js";
import { maskJs } from "./bundle-js.js";
import { shippedSet } from "./bundle-paths.js";
import { EN_OPTIONS_CATALOGUE, OPTIONS_LOCALE, OPTIONS_MESSAGE_KEYS } from "./options-strings.js";

/** Every `{placeholder}` a template asks for. */
function placeholdersIn(template: string): string[] {
	return [...template.matchAll(/\{(\w+)\}/g)].map((match) => match[1] ?? "");
}
import { ALLOWED_HOST_PERMISSIONS, ALLOWED_PERMISSIONS } from "./permissions.js";

const pkgRoot = join(dirname(fileURLToPath(import.meta.url)), "..");
const repoRoot = join(pkgRoot, "..", "..");

const page = readFileSync(join(pkgRoot, "options.html"), "utf8");
const stylesheet = readFileSync(join(pkgRoot, "options.css"), "utf8");
const manifestText = readFileSync(join(pkgRoot, "manifest.json"), "utf8");
const manifest = JSON.parse(manifestText) as Record<string, unknown>;
const worker = readFileSync(join(pkgRoot, "service-worker.js"), "utf8");
const tokens = readFileSync(join(repoRoot, "packages", "ui-kit", "css", "tokens.css"), "utf8");

/**
 * The worker with its comments blanked, and the stylesheet with its own.
 *
 * Both files *discuss* the things the tests below forbid — the worker's
 * comment explains why `chrome.tabs.create` was not used, and the stylesheet's
 * header names the colour literals it must not contain. A gate that read the
 * raw text would be reading the prose, so the same masking the bundled-only
 * gate uses (`maskJs`) is applied here rather than a hand-rolled strip.
 */
const workerCode = maskJs(worker).text;
const stylesheetCode = stylesheet.replace(/\/\*[\s\S]*?\*\//g, " ");

/** The page with its HTML comments blanked, so prose in a comment is prose. */
const pageWithoutComments = page.replace(/<!--[\s\S]*?-->/g, (comment) =>
	comment.replace(/[^\n]/g, " "),
);

/** Every `data-i18n` / `data-i18n-aria-label` key the markup asks for. */
function i18nKeysIn(html: string): string[] {
	return [...html.matchAll(/data-i18n(?:-aria-label)?="([^"]+)"/g)].map((match) => match[1] ?? "");
}

describe("the manifest names a page that ships (SL-4.EXT.07)", () => {
	it("declares options_page, and it is a packaged file", () => {
		expect(manifest.options_page).toBe("options.html");
		expect(shippedSet().has("options.html")).toBe(true);
		expect(shippedSet().has("options.css")).toBe(true);
	});

	it("passes the manifest gate with the options page in it", () => {
		// `options_page` is a `PACKAGE_PATH_FIELDS` key, so this is where a
		// manifest pointing at a page the packer never copied fails.
		expect(findManifestViolations(manifestText, shippedSet())).toEqual([]);
	});

	it("requests no permission beyond the approved set", () => {
		expect(manifest.permissions).toEqual(ALLOWED_PERMISSIONS);
		expect(manifest.host_permissions).toEqual(ALLOWED_HOST_PERMISSIONS);
		expect(manifest.host_permissions).toEqual([]);
	});
});

describe("the page itself (SL-4.EXT.07)", () => {
	it("passes the bundled-only document gate", () => {
		// No inline script, no remote resource, and every relative reference
		// resolving inside the package.
		expect(findHtmlViolations("options.html", page, shippedSet())).toEqual([]);
	});

	it("carries no text of its own", () => {
		// Every run of text between tags must be whitespace. `<title>` is the one
		// deliberate exception and it is asserted separately, against the
		// manifest, so there is one definition of the product's name.
		const withoutTitle = pageWithoutComments.replace(
			/<title>[\s\S]*?<\/title>/g,
			"<title></title>",
		);
		for (const [, text] of withoutTitle.matchAll(/>([^<]*)</g)) {
			expect((text ?? "").trim(), `literal text in the page: ${JSON.stringify(text)}`).toBe("");
		}
	});

	it("asks only for keys the catalogue has", () => {
		for (const key of i18nKeysIn(page)) {
			expect(OPTIONS_MESSAGE_KEYS, `${key} is not in the catalogue`).toContain(key);
		}
		expect(i18nKeysIn(page).length).toBeGreaterThan(15);
	});

	it("asks only for keys with no placeholder, so the generic fill can resolve them", () => {
		// The bug this caught: `data-i18n="options.page.heading"` on the `h1`, whose
		// template takes `{product}`. The generic pass has no values to give it, and
		// `formatMessage` deliberately leaves an unfilled placeholder visible — so
		// the page's first line read "{product} options". A templated key has to be
		// filled by the boot, from an id, and this is what keeps the two apart.
		const catalogue = new Map<string, string>(
			OPTIONS_MESSAGE_KEYS.map((key) => [key, EN_OPTIONS_CATALOGUE[key]] as const),
		);
		for (const key of i18nKeysIn(page)) {
			const template = catalogue.get(key) ?? "";
			expect(
				placeholdersIn(template),
				`${key} has a placeholder but is filled generically`,
			).toEqual([]);
		}
		// And the two that do take a value are filled by id, so they are not
		// `data-i18n` keys at all.
		expect(page).toContain('id="selis-title"');
		expect(page).toContain('id="selis-version"');
	});

	it("titles itself with the manifest's product name", () => {
		const name = (manifest as { name: string }).name;
		expect(page).toContain(`<title>${name}</title>`);
		expect(page).toContain(`<html lang="${OPTIONS_LOCALE}">`);
	});

	it("reaches no capability the extension does not have", () => {
		// `chrome.tabs` would need sign-off EXT.01 refused; a URL would be
		// something the store reviewer reads as remote surface; a fetch would be
		// the one network request this page has no business making.
		for (const [name, source] of [
			["options.html", pageWithoutComments],
			["options.css", stylesheetCode],
			["service-worker.js", workerCode],
		] as const) {
			for (const forbidden of [
				"chrome.tabs",
				"chrome.permissions",
				"chrome.scripting",
				"XMLHttpRequest",
				"fetch(",
				"http://",
				"https://",
			]) {
				expect(source, `${name} names ${forbidden}`).not.toContain(forbidden);
			}
		}
	});

	it("keeps the accessibility structure the copy depends on", () => {
		// Every control is labelled, every section is named, and there is one
		// live region for outcomes. A page that loses these is a page a screen
		// reader cannot use, and the copy is the point of the page.
		expect(page).toContain('for="selis-telemetry"');
		expect(page).toContain('aria-describedby="selis-telemetry-help"');
		expect(page).toContain('role="status"');
		expect(page).toContain('aria-live="polite"');
		expect(page).toContain('aria-labelledby="selis-welcome-heading"');
		expect(page).toContain('tabindex="-1"');
		// The welcome guide is hidden in the markup; the boot reveals it.
		expect(page).toMatch(/id="selis-welcome"[\s\S]*?hidden/);
	});
});

describe("the stylesheet (SL-4.EXT.07)", () => {
	it("hard-codes no colour", () => {
		// Every colour on the page is a token, so the four themes in
		// `tokens.css` reach it and none of them is overridden here. Read from
		// the comment-stripped sheet: this file's own header names the literals
		// it forbids, and prose is not a declaration.
		for (const literal of [/#[\da-fA-F]{3,8}\b/, /\brgba?\(/, /\bhsla?\(/]) {
			expect(stylesheetCode, `colour literal ${literal}`).not.toMatch(literal);
		}
		// `transparent` and `currentColor` are keywords, not values, and are the
		// only colour words allowed to appear.
		const words = stylesheetCode
			.split(/[;{]/)
			.map((declaration) => declaration.split(":")[1] ?? "")
			.join(" ");
		for (const named of ["white", "black", "red", "blue", "grey", "gray"]) {
			expect(words, `named colour ${named}`).not.toContain(named);
		}
	});

	it("names only tokens that tokens.css actually defines", () => {
		// The check that stops a renamed role from being written here and
		// silently resolving to nothing: `--selis-color-surface` and the other
		// pre-UI.14 names do not exist, and this is what says so.
		const defined = new Set(
			[...tokens.matchAll(/^\s*(--selis-[\w-]+)\s*:/gm)].map((match) => match[1] ?? ""),
		);
		expect(defined.size).toBeGreaterThan(50);
		const used = [...stylesheet.matchAll(/var\((--selis-[\w-]+)\)/g)].map(
			(match) => match[1] ?? "",
		);
		expect(used.length).toBeGreaterThan(10);
		for (const name of used) {
			expect(defined, `${name} is not defined by tokens.css`).toContain(name);
		}
	});

	it("keeps [hidden] winning over the page's own display rules", () => {
		// `.selis-section` sets `display: flex`, which beats the user-agent's
		// `[hidden] { display: none }` at equal specificity. Without an explicit
		// rule here the welcome guide is always visible and the first-run flag
		// controls nothing at all.
		expect(stylesheet).toMatch(/\.selis-section\[hidden\]\s*\{\s*display:\s*none/);
	});

	it("is linked from the page, and ships", () => {
		expect(page).toContain('href="./options.css"');
		expect(page).toContain('href="./ui-kit/css/tokens.css"');
		expect(page).toContain('href="./ui-kit/css/base.css"');
		// The generated token sheet is UI.14's; a copy living here would drift.
		expect(stylesheet).not.toContain("--selis-color-bg:");
	});
});

describe("the first-run trigger in the worker (SL-4.EXT.07)", () => {
	it("opens the options page and nothing else", () => {
		// `openOptionsPage` needs no permission and cannot be pointed anywhere.
		// `chrome.tabs.create` would work too, but it is the API EXT.01 denied and
		// a reviewer reads it as a reach the extension does not have.
		expect(worker).toContain("chrome.runtime.openOptionsPage()");
		expect(worker).toContain("planFirstRun(");
		expect(workerCode).not.toContain("chrome.tabs");
	});

	it("still installs the interception rules on every install event", () => {
		// The first-run hook was added next to the existing one; the ruleset must
		// not have become conditional on it.
		expect(worker).toMatch(
			/onInstalled\.addListener\([\s\S]*?installInterception\(\)[\s\S]*?offerWelcomeGuide\(details\)/,
		);
	});
});
