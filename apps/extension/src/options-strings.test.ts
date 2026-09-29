/**
 * SL-4.EXT.07 — the options page's catalogue, checked as a claim.
 *
 * Three things are asserted here that a test of "the strings load" would not
 * catch, and all three are about the page telling the truth:
 *
 * 1. **The catalogue is complete.** Every key has a template, every template's
 *    placeholders are ones the page can fill, and the page's markup only asks
 *    for keys that exist. A key with no sentence is a blank line in front of a
 *    user.
 * 2. **The telemetry help says the port is inert.** The switch is real and the
 *    port behind it records nothing; a page that implied otherwise would be a
 *    false privacy disclosure, and SL-4.EXT.11 has to defend "does not collect
 *    user data" to a store reviewer.
 * 3. **The capability list covers the approved permission set exactly.** A
 *    permission added to the manifest without a sentence here fails this file,
 *    which is what makes the addition a conversation rather than a quiet commit.
 */

import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { describe, expect, it } from "vitest";
import {
	DEFAULT_PRODUCT_NAME,
	EN_OPTIONS_CATALOGUE,
	OPTIONS_LOCALE,
	OPTIONS_MESSAGE_KEYS,
	PERMISSION_MESSAGE_KEYS,
	createOptionsCatalogue,
	createOptionsFormatter,
	formatMessage,
	formatMibibytes,
	hostAccessRows,
	permissionMessageKeys,
} from "./options-strings.js";
import { ALLOWED_HOST_PERMISSIONS, ALLOWED_OPTIONAL_HOST_PERMISSIONS, ALLOWED_PERMISSIONS } from "./permissions.js";

const pkgRoot = join(dirname(fileURLToPath(import.meta.url)), "..");

/** Every `{placeholder}` a template asks for. */
function placeholdersIn(template: string): string[] {
	return [...template.matchAll(/\{(\w+)\}/g)].map((match) => match[1] ?? "");
}

describe("the catalogue is complete (SL-4.EXT.07)", () => {
	it("has a non-empty template for every key, and no template without a key", () => {
		const catalogueKeys = Object.keys(EN_OPTIONS_CATALOGUE).sort();
		expect(catalogueKeys).toEqual([...OPTIONS_MESSAGE_KEYS].sort());
		for (const key of OPTIONS_MESSAGE_KEYS) {
			expect(EN_OPTIONS_CATALOGUE[key].trim().length, key).toBeGreaterThan(0);
		}
	});

	it("asks only for placeholders the page can fill", () => {
		// Every placeholder the page passes, in one place. A template asking for
		// anything else would render as `{resident}` in front of a user, which is
		// the failure `formatMessage`'s unknown-placeholder rule exists to make
		// loud.
		const fillable = new Set(["product", "version", "pattern", "resident", "budget"]);
		for (const key of OPTIONS_MESSAGE_KEYS) {
			for (const name of placeholdersIn(EN_OPTIONS_CATALOGUE[key])) {
				expect(fillable.has(name), `${key} asks for {${name}}`).toBe(true);
			}
		}
	});

	it("merges an override over English rather than replacing the catalogue", () => {
		const overridden = createOptionsCatalogue({ "options.welcome.heading": "Willkommen" });
		expect(overridden["options.welcome.heading"]).toBe("Willkommen");
		expect(overridden["options.privacy.heading"]).toBe(
			EN_OPTIONS_CATALOGUE["options.privacy.heading"],
		);
		expect(Object.keys(overridden).length).toBe(OPTIONS_MESSAGE_KEYS.length);
	});

	it("substitutes by name and leaves an unknown placeholder visible", () => {
		const format = createOptionsFormatter(createOptionsCatalogue());
		expect(format("options.documentTitle", { product: "Selis PDF Viewer" })).toBe(
			"Selis PDF Viewer — Options",
		);
		// A typo must look wrong where somebody can see it, not read as blank.
		expect(formatMessage("{a} and {b}", { a: "x" })).toBe("x and {b}");
	});

	it("declares the locale the page's lang attribute is set from", () => {
		expect(OPTIONS_LOCALE).toBe("en");
	});
});

describe("the page tells the truth (SL-4.EXT.07)", () => {
	it("says the telemetry port records nothing, in the string itself", () => {
		const help = EN_OPTIONS_CATALOGUE["options.telemetry.help"].toLowerCase();
		// The port behind the switch is inert (EXT.06): no endpoint, no
		// permission to reach one. The sentence has to say so, because the
		// switch being present is otherwise read as "this is being collected".
		expect(help).toContain("nothing is recorded");
		expect(help).toContain("no analytics endpoint");
		// And it must not claim the opposite either.
		for (const claim of ["we collect", "helps us improve", "anonymous statistics about your"]) {
			expect(help, claim).not.toContain(claim);
		}
	});

	it("states the measured CJK refusal rather than promising the feature", () => {
		const status = EN_OPTIONS_CATALOGUE["options.cjk.status"];
		expect(status).toContain("{resident}");
		expect(status).toContain("{budget}");
		expect(status).toContain("refused");
		for (const promise of ["coming soon", "download", "install the fonts"]) {
			expect(status, promise).not.toContain(promise);
		}
	});

	it("does not promise editing, and describes the local-file flow as it is", () => {
		// The build is read-only and `file://` is EXT.09's scope. Copy that
		// implies otherwise is a support ticket on day one.
		//
		// SL-4.EXT.09 changed this sentence, and the direction of the change is the
		// point: EXT.07 said local files "are not opened by the extension … it has
		// not shipped yet", which was true then and would have been a lie the moment
		// the flow landed. It now says what is actually true — a conditional grant the
		// reader controls, and a permission-free route that always works.
		const welcome = OPTIONS_MESSAGE_KEYS.map((key) => EN_OPTIONS_CATALOGUE[key]).join(" ");
		expect(welcome).toContain("cannot edit, sign, or save one back yet");
		expect(welcome).toContain("asks for nothing until you turn it on");
		expect(welcome).toContain("needs no permission at all");
		// And it must not have become a promise the extension cannot keep.
		for (const stale of ["not opened by the extension", "has not shipped yet"]) {
			expect(welcome, stale).not.toContain(stale);
		}
	});
});

describe("the capability list (SL-4.EXT.07)", () => {
	it("explains exactly the approved permissions, and nothing else", () => {
		// The sign-off gate for a new permission: `manifest.test.ts` fails on the
		// manifest, this fails on the page that would then show a blank row.
		expect(Object.keys(PERMISSION_MESSAGE_KEYS).sort()).toEqual([...ALLOWED_PERMISSIONS].sort());
		for (const key of Object.values(PERMISSION_MESSAGE_KEYS)) {
			expect(OPTIONS_MESSAGE_KEYS).toContain(key);
			expect(EN_OPTIONS_CATALOGUE[key].length).toBeGreaterThan(0);
		}
	});

	it("reports a permission it has no sentence for rather than skipping it", () => {
		// `null`, not a filtered-out row: the page then holds a key it cannot
		// fill, and the test above is what notices.
		expect(permissionMessageKeys(["storage", "tabs"])).toEqual([
			"options.permission.storage",
			null,
		]);
		expect(permissionMessageKeys()).toHaveLength(ALLOWED_PERMISSIONS.length);
	});

	it("says there is no website access, because there is none", () => {
		expect(ALLOWED_HOST_PERMISSIONS).toEqual([]);
		const rows = hostAccessRows(ALLOWED_HOST_PERMISSIONS);
		expect(rows).toEqual([{ pattern: "", key: "options.permissions.hosts.none" }]);
		const format = createOptionsFormatter(EN_OPTIONS_CATALOGUE);
		const sentence = format(rows[0]?.key ?? "options.permissions.hosts.none");
		expect(sentence).toContain("No website access at all");
	});

	it("renders one row per pattern if a future task adds a host permission", () => {
		const rows = hostAccessRows(["https://example.com/*"]);
		expect(rows).toEqual([{ pattern: "https://example.com/*", key: "options.permissions.host" }]);
	});

	it("gives a declared-but-ungranted pattern its own sentence (SL-4.EXT.09)", () => {
		// The load-bearing honesty check: the page says what the browser has
		// allowed, so the granted sentence must never be used for a pattern nobody
		// has granted. And the "no website access" row stays, because an ungranted
		// optional pattern is not access.
		const rows = hostAccessRows(ALLOWED_HOST_PERMISSIONS, ALLOWED_OPTIONAL_HOST_PERMISSIONS);
		expect(rows).toEqual([
			{ pattern: "", key: "options.permissions.hosts.none" },
			{ pattern: "file:///", key: "options.permissions.host.optional" },
		]);
		const format = createOptionsFormatter(EN_OPTIONS_CATALOGUE);
		const optional = format("options.permissions.host.optional", { pattern: "file:///" });
		expect(optional).toContain("Not granted");
		expect(optional).toContain("file:///");
		expect(optional).toContain("needs no permission at all");
		// The two rows must not be the same sentence wearing different keys.
		expect(optional).not.toBe(format("options.permissions.hosts.none"));
		expect(format("options.permissions.hosts.none")).toContain("No website access at all");
	});

	it("still answers with the granted sentence alone when nothing is declared optional", () => {
		// A host that passes no optional list gets exactly the pre-EXT.09 answer,
		// rather than a stray empty row.
		expect(hostAccessRows([])).toEqual([
			{ pattern: "", key: "options.permissions.hosts.none" },
		]);
	});
});

describe("the numbers on the page (SL-4.EXT.07)", () => {
	it("formats sizes in MiB, one decimal, as every budget on this page is stated", () => {
		expect(formatMibibytes(8 * 1024 * 1024)).toBe("8.0 MiB");
		expect(formatMibibytes(11_162_268)).toBe("10.6 MiB");
	});

	it("keeps the fallback product name equal to the manifest's", () => {
		// One definition of the product's name. The page prefers
		// `chrome.runtime.getManifest().name`; this is what it falls back to.
		const manifest = JSON.parse(readFileSync(join(pkgRoot, "manifest.json"), "utf8")) as {
			name: string;
		};
		expect(DEFAULT_PRODUCT_NAME).toBe(manifest.name);
	});
});
