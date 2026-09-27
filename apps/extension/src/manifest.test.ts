/**
 * SL-4.EXT.02 — the PDF-serving matrix as executable assertions.
 *
 * Every row of `PATTERN_MATRIX` is checked against the *shipped* ruleset, so
 * the matrix cannot rot into documentation: if a rule change regresses a
 * pattern, this suite fails naming the pattern. The `cannot-work` rows are
 * additionally asserted to stay un-intercepted, which is the DoD's "document
 * the cases that cannot work" clause made mechanical — a future permission
 * grant that starts serving one of them fails here and forces the matrix to
 * be updated deliberately.
 */

import { describe, expect, it } from "vitest";
import {
	ALLOWED_HOST_PERMISSIONS,
	DENIED_PERMISSIONS,
	PATTERN_MATRIX,
	RULE_ID,
	SRC_PARAM,
	UNSUPPORTED_PATTERNS,
	VIEWER_PATH,
	buildRedirectRules,
	isIntercepted,
	isRequestIntercepted,
	viewerUrlFor,
} from "./permissions.js";

describe("EXT.02 DNR ruleset shape", () => {
	it("redirects main_frame navigations to the bundled viewer only", () => {
		const rules = buildRedirectRules();
		expect(rules.length).toBeGreaterThan(0);
		for (const rule of rules) {
			expect(rule.action.type).toBe("redirect");
			expect(rule.action.redirect.extensionPath).toBe(VIEWER_PATH);
			// main_frame only: sub-resources and fetches are never hijacked.
			expect(rule.condition.resourceTypes).toEqual(["main_frame"]);
			// Chrome refuses a ruleset regex over 2 KB; ours must stay under.
			expect(rule.condition.regexFilter.length).toBeLessThan(2048);
		}
	});

	it("uses disjoint, stable rule ids", () => {
		const ids = buildRedirectRules().map((r) => r.id);
		expect(new Set(ids).size).toBe(ids.length);
		expect(ids).toContain(RULE_ID.pdfPath);
		expect(ids).toContain(RULE_ID.pdfQuery);
	});

	it("claims no URL with two rules at once", () => {
		const rules = buildRedirectRules();
		for (const { name, url } of PATTERN_MATRIX) {
			const hits = rules.filter((r) => new RegExp(r.condition.regexFilter).test(url));
			expect(hits.length, `${name}: matched by ${hits.length} rules`).toBeLessThanOrEqual(1);
		}
	});

	it("uses no condition that would require host access to evaluate", () => {
		// responseHeaders / initiatorDomains conditions are unreachable without
		// host permissions; a rule that added one would silently never fire.
		for (const rule of buildRedirectRules()) {
			const keys = Object.keys(rule.condition);
			expect(keys).not.toContain("responseHeaders");
			expect(keys).not.toContain("initiatorDomains");
		}
	});
});

describe("EXT.02 the original URL reaches the viewer intact", () => {
	const paramsOf = (doc: string): URLSearchParams => {
		const viewer = viewerUrlFor(doc);
		return new URLSearchParams(viewer.slice(viewer.indexOf("?") + 1));
	};

	it("carries the document URL as an encoded parameter", () => {
		const doc = "https://example.com/a.pdf";
		expect(viewerUrlFor(doc)).toBe(`${VIEWER_PATH}?${SRC_PARAM}=${encodeURIComponent(doc)}`);
	});

	it("survives a document URL carrying its own query and fragment", () => {
		const doc = "https://example.com/a.pdf?x=1&y=2#page=3";
		// Round-trips exactly: the fragment did not truncate it, and the
		// document's own parameters did not become viewer parameters.
		expect(paramsOf(doc).get(SRC_PARAM)).toBe(doc);
		expect(paramsOf(doc).getAll(SRC_PARAM)).toHaveLength(1);
	});

	it("cannot be used to forge a second src parameter", () => {
		const hostile = "https://evil.test/x.pdf?src=https://evil.test/other.pdf";
		expect(paramsOf(hostile).getAll(SRC_PARAM)).toHaveLength(1);
		expect(paramsOf(hostile).get(SRC_PARAM)).toBe(hostile);
	});
});

describe("EXT.02 the ~30-pattern matrix", () => {
	it("covers at least 30 real-world PDF-serving patterns", () => {
		expect(PATTERN_MATRIX.length).toBeGreaterThanOrEqual(30);
	});

	it("gives every pattern a distinct name and a real reason", () => {
		const names = PATTERN_MATRIX.map((p) => p.name);
		expect(new Set(names).size).toBe(names.length);
		for (const p of PATTERN_MATRIX) {
			expect(p.url.length, p.name).toBeGreaterThan(0);
			expect(p.reason.length, p.name).toBeGreaterThan(20);
		}
	});

	it("matches every row's recorded verdict against the shipped rules", () => {
		const failures: string[] = [];
		for (const row of PATTERN_MATRIX) {
			// Rows that name a resourceType are asking about the *request*;
			// the rest are about a top-level navigation.
			const actually = row.resourceType
				? isRequestIntercepted(row.url, row.resourceType)
				: isRequestIntercepted(row.url, "main_frame");
			if (actually !== (row.outcome === "intercepted")) {
				failures.push(
					`${row.name} (${row.url} as ${row.resourceType ?? "main_frame"}): recorded "${row.outcome}" but the ruleset says "${actually ? "intercepted" : "not intercepted"}"`,
				);
			}
		}
		expect(failures, failures.join("\n")).toEqual([]);
	});

	it("shows the resource-type dimension is real, not decorative", () => {
		// The same .pdf URL, intercepted as a navigation and untouched as a
		// fetch. If main_frame-only ever loosened, this fails.
		const url = "https://example.com/docs/report.pdf";
		expect(isRequestIntercepted(url, "main_frame")).toBe(true);
		expect(isRequestIntercepted(url, "xmlhttprequest")).toBe(false);
		expect(isRequestIntercepted(url, "sub_frame")).toBe(false);
	});

	it("names the capability each unsupported pattern would need", () => {
		expect(UNSUPPORTED_PATTERNS.length).toBeGreaterThan(0);
		for (const p of UNSUPPORTED_PATTERNS) {
			expect(p.reason, p.name).toMatch(
				/host permission|scheme is outside|not a network request|no host permission|response/i,
			);
			// None of them is silently interceptable.
			expect(isIntercepted(p.url), p.name).toBe(false);
		}
	});

	it("covers the DoD's named cases explicitly", () => {
		const has = (needle: string): boolean =>
			PATTERN_MATRIX.some((p) => p.name.toLowerCase().includes(needle));
		// direct .pdf, Content-Type-only, Content-Disposition: inline,
		// redirects, and POST-produced PDFs each appear as a named row.
		expect(has("direct .pdf")).toBe(true);
		expect(has("content-type: application/pdf, extensionless")).toBe(true);
		expect(has("content-disposition: inline")).toBe(true);
		expect(has("302 redirect")).toBe(true);
		expect(has("post form submit")).toBe(true);
	});
});

describe("EXT.02 permission minimisation is preserved", () => {
	it("widens no host permissions to reach the matrix", () => {
		// Every cannot-work row is a consequence of holding no host
		// permissions. If this ever changes, the matrix must change too.
		expect(ALLOWED_HOST_PERMISSIONS).toEqual([]);
		expect(DENIED_PERMISSIONS).toContain("webRequest");
		expect(DENIED_PERMISSIONS).toContain("webRequestBlocking");
	});
});

/**
 * SL-4.EXT.01 manifest gate: MV3, minimal permissions, no broad hosts, no
 * remote code, and every shipped permission justified in PERMISSIONS.md.
 * `manifest.json` is the artefact; `permissions.ts` is the contract; this
 * suite pins the two together so scope creep fails CI instead of review.
 */

import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { ALLOWED_PERMISSIONS, DENIED_HOST_PATTERNS, validateManifest } from "./permissions.js";

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
