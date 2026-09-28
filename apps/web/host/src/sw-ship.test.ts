/**
 * What actually ships (SL-4.WEB.02).
 *
 * A caching policy that is never registered is a policy that never runs, so the
 * deployment shape is part of the gate: the page registers the worker, the
 * worker is served from the site root, every precached URL is a file that
 * exists, and the boot script's SRI digest is the one the deployment pins.
 *
 * That last one closes a gap WEB.01 recorded honestly: digests were hand-computed
 * there, so a stale one failed loudly in the browser and nowhere else. For the
 * boot script it is now checked against the file on every test run.
 */

import { existsSync, readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { describe, expect, it } from "vitest";
import { collectAssetReferences } from "./sri.js";
import { computeSri } from "./sri.js";
import { PRECACHE_PATHS, SERVICE_WORKER_PATH, SW_SCOPE } from "./sw-policy.js";

const pkgRoot = join(dirname(fileURLToPath(import.meta.url)), "..");
const html = readFileSync(join(pkgRoot, "public", "index.html"), "utf8");
const boot = readFileSync(join(pkgRoot, "public", "assets", "boot.js"), "utf8");
const packScript = readFileSync(join(pkgRoot, "tools", "place-sw.mjs"), "utf8");

describe("the deployment serves what the policy expects", () => {
	it("has a file behind every precached URL, so install cannot fail in the field", () => {
		for (const path of PRECACHE_PATHS) {
			const file = path === "/" ? "index.html" : path.replace(/^\//, "");
			expect(existsSync(join(pkgRoot, "public", file)), path).toBe(true);
		}
	});

	it("serves the worker from the site root, which is what the scope requires", () => {
		// A worker served from /assets/ could not take scope "/", and a scope
		// narrower than the app would leave /wasm/… uncached.
		expect(SERVICE_WORKER_PATH).toBe("/sw.js");
		expect(SW_SCOPE).toBe("/");
		expect(SERVICE_WORKER_PATH.startsWith(SW_SCOPE)).toBe(true);
	});

	it("copies the worker and the page-side registrar into the deployment root", () => {
		// `public/` is the deploy root (public/_headers, vercel.json), so both
		// files have to be placed there or the registration 404s.
		expect(packScript).toContain('"sw.js"');
		expect(packScript).toContain('"sw-register.js"');
		expect(packScript).toContain('join(pkgRoot, "public")');
	});
});

describe("the page registers the worker", () => {
	it("boot.js registers it, from a same-origin specifier only", () => {
		expect(boot).toContain("registerServiceWorker");
		const specifiers: string[] = [...boot.matchAll(/from\s+"([^"]+)"/g)]
			.map((match) => match[1] ?? "")
			.filter((specifier) => specifier.length > 0);
		expect(specifiers).toEqual(["/sw-register.js"]);
		for (const specifier of specifiers) {
			expect(specifier.startsWith("/"), specifier).toBe(true);
			expect(specifier.includes(".."), specifier).toBe(false);
		}
	});

	it("loads boot.js as a module, because it has an import", () => {
		const tag = collectAssetReferences(html).find((ref) => ref.src === "/assets/boot.js");
		expect(tag, "boot.js must be referenced by index.html").toBeDefined();
		expect(html).toMatch(/<script[^>]*src="\/assets\/boot\.js"[^>]*type="module"/);
	});

	it("pins the digest of the boot script it ships", () => {
		// Recomputed whenever boot.js changes. A mismatch here is a page that
		// would fail SRI in the browser and nowhere else.
		const digest = computeSri(
			new Uint8Array(readFileSync(join(pkgRoot, "public", "assets", "boot.js"))),
		);
		expect(html).toContain(`integrity="${digest}"`);
	});
});
