/**
 * SL-4.EXT.01 manifest gate: MV3, minimal permissions, no broad hosts, no
 * remote code, and every shipped permission justified in PERMISSIONS.md.
 * `manifest.json` is the artefact; `permissions.ts` is the contract; this
 * suite pins the two together so scope creep fails CI instead of review.
 */

import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { describe, expect, it } from "vitest";
import {
	ALLOWED_HOST_PERMISSIONS,
	ALLOWED_PERMISSIONS,
	DENIED_HOST_PATTERNS,
	validateManifest,
} from "./permissions.js";

const pkgRoot = join(dirname(fileURLToPath(import.meta.url)), "..");

function loadManifest(): Record<string, unknown> {
	return JSON.parse(readFileSync(join(pkgRoot, "manifest.json"), "utf8")) as Record<
		string,
		unknown
	>;
}

describe("EXT.01 MV3 minimal-permission manifest", () => {
	it("passes the minimal-permission contract with no violations", () => {
		const violations = validateManifest(loadManifest());
		expect(violations, JSON.stringify(violations, null, 2)).toEqual([]);
	});

	it("requests exactly the approved set (no silent additions)", () => {
		const manifest = loadManifest();
		const permissions = (manifest.permissions as string[]).slice().sort();
		expect(permissions).toEqual(ALLOWED_PERMISSIONS.slice().sort());
		expect(manifest.host_permissions).toEqual(ALLOWED_HOST_PERMISSIONS);
	});

	it("ships no broad host patterns", () => {
		const raw = readFileSync(join(pkgRoot, "manifest.json"), "utf8");
		for (const pattern of DENIED_HOST_PATTERNS) {
			expect(raw, `broad host pattern ${pattern}`).not.toContain(`"${pattern}"`);
		}
	});

	it("contains no remote code references (ADR-P0028, EXT.04 precursor)", () => {
		const raw = readFileSync(join(pkgRoot, "manifest.json"), "utf8");
		expect(raw).not.toMatch(/https?:\/\/cdn\./);
		expect(raw).not.toContain("unsafe-eval");
		expect(raw).not.toContain("unsafe-inline");
		const worker = join(pkgRoot, "service-worker.js");
		const workerSrc = readFileSync(worker, "utf8");
		expect(workerSrc).not.toMatch(/https?:\/\//);
		expect(workerSrc).not.toContain("eval(");
		expect(workerSrc).not.toContain("new Function");
		expect(workerSrc).not.toContain("importScripts");
	});

	it("justifies every shipped permission in PERMISSIONS.md", () => {
		const doc = readFileSync(join(pkgRoot, "PERMISSIONS.md"), "utf8");
		for (const permission of ALLOWED_PERMISSIONS) {
			expect(doc, `missing PERMISSIONS.md row for ${permission}`).toContain(`\`${permission}\``);
		}
		expect(doc).toContain("host_permissions");
		expect(doc).toContain("Deliberately NOT requested");
	});

	it("rejects scope creep through the validator (guardrail sanity)", () => {
		expect(
			validateManifest({
				manifest_version: 3,
				permissions: ["declarativeNetRequest", "offscreen", "tabs"],
				host_permissions: [],
				background: { service_worker: "service-worker.js" },
				content_security_policy: { extension_pages: "script-src 'self'; object-src 'self';" },
			}).length,
		).toBeGreaterThan(0);
		expect(
			validateManifest({
				manifest_version: 2,
				permissions: ["declarativeNetRequest", "offscreen"],
				host_permissions: [],
				background: { service_worker: "service-worker.js" },
				content_security_policy: { extension_pages: "script-src 'self'; object-src 'self';" },
			}).some((violation) => violation.field === "manifest_version"),
		).toBe(true);
	});
});
