/**
 * SL-4.EXT.09 — the shipped viewer page, the stylesheet it links, and the
 * manifest that names it.
 *
 * This is the same kind of test as `options-page.test.ts`: it reads the **files
 * the store upload will contain** rather than the TypeScript that produced them,
 * because every claim this page makes is a claim about bytes. The local-file
 * flow's whole value is that it *says* something — in the right words, in the
 * right states, with the right controls reachable — and none of that survives if
 * the markup drifts from the catalogue or the sheet drifts from the tokens.
 *
 * | Assertion | The failure it prevents |
 * |---|---|
 * | `default_popup` is declared and names a shipped file | the manifest points at a page the packer never copied — the EXT.03 failure mode |
 * | the page has no text outside `data-i18n` keys | an English literal in markup: untranslatable, and invisible to the catalogue test |
 * | every key the markup asks for exists in `VIEWER_MESSAGE_KEYS` | a blank line, or worse a `viewer.local.denied`, in front of a reader |
 * | every `viewer.local.*` key is *used* | copy shipped for a state the page cannot reach — the silent failure this task exists to remove |
 * | the panel is `hidden` in the markup and loses to its own `display` | a form asking every web-PDF reader to switch a browser setting |
 * | the settings link carries no `href` until boot | a link that goes nowhere, or to a list of every extension |
 * | both routes are present and both named | a dead end where the reader declined |
 * | no colour literal in the sheet, and every token it names is real | a hard-coded colour, or a renamed role resolving to nothing |
 * | the flow reaches no capability the extension does not have | scope creep past the EXT.01 review |
 */

import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { describe, expect, it } from "vitest";
import { findHtmlViolations, findManifestViolations } from "./bundle-docs.js";
import { maskJs } from "./bundle-js.js";
import { shippedSet } from "./bundle-paths.js";
import { FILE_ACCESS_TOGGLE } from "./local-files.js";
import { ALLOWED_HOST_PERMISSIONS, ALLOWED_OPTIONAL_HOST_PERMISSIONS } from "./permissions.js";
import { EN_VIEWER_CATALOGUE, VIEWER_MESSAGE_KEYS } from "./viewer-strings.js";

const pkgRoot = join(dirname(fileURLToPath(import.meta.url)), "..");
const repoRoot = join(pkgRoot, "..", "..");

const page = readFileSync(join(pkgRoot, "viewer.html"), "utf8");
const stylesheet = readFileSync(join(pkgRoot, "viewer.css"), "utf8");
const manifestText = readFileSync(join(pkgRoot, "manifest.json"), "utf8");
const manifest = JSON.parse(manifestText) as Record<string, unknown>;
const tokens = readFileSync(join(repoRoot, "packages", "ui-kit", "css", "tokens.css"), "utf8");

/** The page with its HTML comments blanked, so prose in a comment is prose. */
const pageWithoutComments = page.replace(/<!--[\s\S]*?-->/g, (comment) =>
	comment.replace(/[^\n]/g, " "),
);

/** The sheet with its own comments blanked, for the same reason. */
const stylesheetCode = stylesheet.replace(/\/\*[\s\S]*?\*\//g, " ");

/** Every `data-i18n` key the markup asks for. */
function i18nKeysIn(html: string): string[] {
	return [...html.matchAll(/data-i18n(?:-aria-label)?="([^"]+)"/g)].map((match) => match[1] ?? "");
}

/** One module of this package, with its comments blanked. */
function moduleOf(...parts: string[]): string {
	return maskJs(readFileSync(join(pkgRoot, ...parts), "utf8")).text;
}

describe("the manifest names a page that ships (SL-4.EXT.09)", () => {
	it("declares default_popup, and it is a packaged file", () => {
		expect(manifest.action).toMatchObject({ default_popup: "viewer.html" });
		expect(shippedSet().has("viewer.html")).toBe(true);
		expect(shippedSet().has("viewer.css")).toBe(true);
	});

	it("passes the manifest gate with the local-file pattern in it", () => {
		expect(findManifestViolations(manifestText, shippedSet())).toEqual([]);
	});

	it("requests no host access at install, and one optional pattern", () => {
		// The load-bearing claim of the whole task, asserted on the bytes: nothing
		// is granted until the reader acts, and EXT.01's `host_permissions: []`
		// survives untouched.
		expect(manifest.host_permissions).toEqual(ALLOWED_HOST_PERMISSIONS);
		expect(manifest.host_permissions).toEqual([]);
		expect(manifest.optional_host_permissions).toEqual(ALLOWED_OPTIONAL_HOST_PERMISSIONS);
	});
});

describe("the page itself (SL-4.EXT.09)", () => {
	it("passes the bundled-only document gate", () => {
		expect(findHtmlViolations("viewer.html", page, shippedSet())).toEqual([]);
	});

	it("carries no text of its own", () => {
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
			expect(VIEWER_MESSAGE_KEYS, `${key} is not in the catalogue`).toContain(key);
		}
		expect(i18nKeysIn(page).length).toBeGreaterThan(10);
	});

	it("reaches every local-file key, so no state is shipped unreachable", () => {
		// The mirror image of the assertion above, and the one that keeps this task
		// honest: a key nothing can render is a state the reader can reach but the
		// page cannot show, which is precisely how `file://` failed before.
		//
		// A key is reached by **either** of two routes, and both are real. The
		// static sentences are `data-i18n` in the markup; the per-state ones are
		// named in `viewer-boot.ts` and written into an element by id, because they
		// depend on which state the flow is in. Asserting the markup alone would
		// have failed on four keys that are perfectly reachable, and asserting the
		// union means a key dropped from *either* place fails here.
		// `[\w.]` rather than `[a-z.]`: the keys are camelCase (`nothingChosen`,
		// `openSettings`), and a regex that quietly matched only the all-lowercase
		// ones would have "passed" here while proving nothing about the rest.
		const reachable = new Set([
			...i18nKeysIn(page),
			...[...moduleOf("src", "viewer-boot.ts").matchAll(/"(viewer\.[\w.]+)"/g)].map(
				(match) => match[1] ?? "",
			),
		]);
		for (const key of VIEWER_MESSAGE_KEYS.filter((k) => k.startsWith("viewer.local."))) {
			expect(reachable.has(key), `${key} cannot be rendered by anything`).toBe(true);
		}
		// And the panel is not a wall of static text: the states that vary have to
		// come from the boot, or the page would show the same sentence for all of
		// them.
		for (const stateful of [
			"viewer.local.status.off",
			"viewer.local.status.unknown",
			"viewer.local.denied",
			"viewer.local.unreadable",
			"viewer.local.unchecked",
		]) {
			expect(i18nKeysIn(page), `${stateful} is a per-state sentence`).not.toContain(stateful);
		}
	});

	it("has a non-empty template for every key, and no template without a key", () => {
		expect(Object.keys(EN_VIEWER_CATALOGUE).sort()).toEqual([...VIEWER_MESSAGE_KEYS].sort());
		for (const key of VIEWER_MESSAGE_KEYS) {
			expect(String(EN_VIEWER_CATALOGUE[key]).trim().length, key).toBeGreaterThan(0);
		}
	});

	it("says the things this task is answerable for, in the string itself", () => {
		// Four claims a reviewer or a support answer rests on, asserted here rather
		// than trusted to a reader's eye. The wording can change; what cannot change
		// is that the page keeps promising only what is true.
		const all = Object.values(EN_VIEWER_CATALOGUE)
			.map((template) => (typeof template === "string" ? template : template.other))
			.join(" ");
		// Declining costs the reader nothing, and the browser can still open it.
		expect(all).toContain("your browser can still open it");
		expect(all).toContain("has not moved");
		// The permission-free route is real and it needs nothing.
		expect(all).toContain("needs no permission at all");
		// The extension asks for nothing up front.
		expect(all).toContain("asks for nothing until you do");
		// ADR-P0016, which the store review has to defend.
		expect(all).toContain("never uploaded");
	});

	it("fills only `{toggle}` generically, and nothing else generically", () => {
		// `{toggle}` is the browser's own name for the switch, injected at fill
		// time; every other placeholder is a value the boot supplies by id. A second
		// generic placeholder would render as `{name}` in front of a reader.
		for (const [, key] of pageWithoutComments.matchAll(/data-i18n="([^"]+)"/g)) {
			const template = EN_VIEWER_CATALOGUE[key as keyof typeof EN_VIEWER_CATALOGUE];
			const text = typeof template === "string" ? template : (template?.other ?? "");
			const placeholders = [...text.matchAll(/\{(\w+)\}/g)].map((m) => m[1] ?? "");
			expect(
				placeholders.filter((name) => name !== "toggle"),
				key,
			).toEqual([]);
		}
	});

	it("keeps the local-file panel hidden until there is a local file in hand", () => {
		// A web PDF arriving from the DNR redirect must not also show a reader a
		// form asking them to change a browser setting. The boot reveals the panel;
		// the markup must not, and the sheet must not defeat the attribute.
		expect(page).toMatch(/id="selis-local"[\s\S]*?hidden/);
		expect(stylesheet).toMatch(/\.selis-viewer__local\[hidden\]\s*\{\s*display:\s*none/);
	});

	it("ships no href for the settings link, because the id is not known yet", () => {
		// A literal here would either go nowhere or list every extension. The boot
		// fills it from `chrome.runtime.id`; until then the written steps are the
		// route, which is why they are a list item rather than a tooltip.
		const anchor = /<a[^>]*id="selis-local-settings"[^>]*>/.exec(page)?.[0] ?? "";
		expect(anchor).not.toContain("href");
		expect(anchor).toContain('data-i18n="viewer.local.openSettings"');
		expect(page).toContain('data-i18n="viewer.local.steps"');
	});

	it("names the browser's own switch in the steps, not a name of our own", () => {
		// `{toggle}` is filled from `local-files.ts`, so the sentence in the page
		// and the string the code asserts are one definition.
		expect(EN_VIEWER_CATALOGUE["viewer.local.steps"]).toContain("{toggle}");
		expect(FILE_ACCESS_TOGGLE).toBe("Allow access to file URLs");
	});

	it("offers both routes, so declining is a choice and not a dead end", () => {
		for (const id of ["selis-local-choose", "selis-local-check", "selis-local-settings"]) {
			expect(page, id).toContain(`id="${id}"`);
		}
		// "Check again" is what makes "on grant it proceeds" true without a second
		// code path: the same flow, re-run, after the reader comes back.
		expect(page).toContain('data-i18n="viewer.local.check"');
	});

	it("keeps the accessibility structure the copy depends on", () => {
		expect(page).toContain('role="status"');
		expect(page).toContain('aria-live="polite"');
		expect(page).toContain('aria-labelledby="selis-local-heading"');
		expect(page).toContain('tabindex="-1"');
		expect(page).toContain('for="selis-local-url"');
		expect(page).toContain('aria-describedby="selis-local-url-help"');
	});

	it("reaches no capability the extension does not have", () => {
		// `chrome.permissions` is the one this task is most likely to be "helped"
		// with: a runtime request for the file scheme is the tidier-looking
		// implementation and is not how Chrome grants it. The flow must not grow
		// one, and neither must the worker.
		for (const [name, source] of [
			["viewer.html", pageWithoutComments],
			["viewer.css", stylesheetCode],
			["viewer-boot", moduleOf("src", "viewer-boot.ts")],
			["local-files", moduleOf("src", "local-files.ts")],
			["host-env", moduleOf("src", "ext", "host-env.ts")],
		] as const) {
			for (const forbidden of [
				"chrome.permissions",
				"chrome.tabs",
				"chrome.scripting",
				"XMLHttpRequest",
				"eval(",
				"new Function",
			]) {
				expect(source, `${name} names ${forbidden}`).not.toContain(forbidden);
			}
		}
	});
});

describe("the stylesheet (SL-4.EXT.09)", () => {
	it("hard-codes no colour", () => {
		for (const literal of [/#[\da-fA-F]{3,8}\b/, /\brgba?\(/, /\bhsla?\(/]) {
			expect(stylesheetCode, `colour literal ${literal}`).not.toMatch(literal);
		}
		const words = stylesheetCode
			.split(/[;{]/)
			.map((declaration) => declaration.split(":")[1] ?? "")
			.join(" ");
		for (const named of ["white", "black", "red", "blue", "grey", "gray"]) {
			expect(words, `named colour ${named}`).not.toContain(named);
		}
	});

	it("names only tokens that tokens.css actually defines", () => {
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

	it("is linked from the page, and ships", () => {
		expect(page).toContain('href="./viewer.css"');
		expect(page).toContain('href="./ui-kit/css/tokens.css"');
		expect(page).toContain('href="./ui-kit/css/base.css"');
		expect(stylesheet).not.toContain("--selis-color-bg:");
	});
});
