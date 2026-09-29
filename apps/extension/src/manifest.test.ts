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
	ALLOWED_OPTIONAL_HOST_PERMISSIONS,
	DENIED_HOST_PATTERNS,
	DENIED_PERMISSIONS,
	PATTERN_MATRIX,
	RULE_ID,
	SRC_PARAM,
	UNSUPPORTED_PATTERNS,
	VIEWER_PATH,
	buildRedirectRules,
	isFileRule,
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

	it("carries exactly one file:// rule, and it is main_frame only", () => {
		// SL-4.EXT.09. The file rule is what makes "on grant it proceeds" true when
		// the reader navigates to a local PDF themselves, so it exists; and it is
		// one rule, main_frame only, for the same reason the other two are: a
		// sub-resource or a page's own `fetch()` of a local file is not a document
		// the reader navigated to.
		const fileRules = buildRedirectRules().filter(isFileRule);
		expect(fileRules).toHaveLength(1);
		expect(fileRules[0]?.id).toBe(RULE_ID.filePath);
		expect(fileRules[0]?.condition.resourceTypes).toEqual(["main_frame"]);
	});

	it("names isFileRule for exactly the file rule", () => {
		// The service worker's degraded install drops by this predicate, so a
		// predicate that matched the wrong rule would take the web-PDF rules down
		// with the local one — the exact outcome `isFileRule`'s doc says it exists
		// to prevent.
		const rules = buildRedirectRules();
		expect(rules.filter(isFileRule).map((r) => r.id)).toEqual([RULE_ID.filePath]);
		expect(rules.filter((rule) => !isFileRule(rule)).map((r) => r.id)).toEqual([
			RULE_ID.pdfPath,
			RULE_ID.pdfQuery,
		]);
	});
});

describe("EXT.09 the local-file pattern, declared", () => {
	it("is optional, and the required host list is still empty", () => {
		// The claim PERMISSIONS.md makes to a reviewer, asserted on the module
		// rather than only on the manifest, so a future edit to either one has to
		// make both of them move.
		expect(ALLOWED_HOST_PERMISSIONS).toEqual([]);
		expect(ALLOWED_OPTIONAL_HOST_PERMISSIONS).toEqual(["file:///"]);
	});

	it("is spelled the way Chrome documents it: three slashes", () => {
		// "Note that this case requires three slashes, not two." The two-slash
		// spellings are in DENIED_HOST_PATTERNS and are a different, invalid
		// pattern rather than a narrower version of this one.
		expect(ALLOWED_OPTIONAL_HOST_PERMISSIONS[0]?.endsWith("/")).toBe(true);
		expect(ALLOWED_OPTIONAL_HOST_PERMISSIONS[0]?.match(/\//g)).toHaveLength(3);
		for (const pattern of DENIED_HOST_PATTERNS) {
			expect(ALLOWED_OPTIONAL_HOST_PERMISSIONS, pattern).not.toContain(pattern);
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
import { ALLOWED_PERMISSIONS, validateManifest } from "./permissions.js";

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
		// SL-4.EXT.09: one *optional* host pattern, and the required list is still
		// empty. Asserted together because the whole argument for the optional field
		// is that the required one did not move.
		expect(manifest.host_permissions).toEqual([]);
		expect(manifest.optional_host_permissions).toEqual(ALLOWED_OPTIONAL_HOST_PERMISSIONS);
	});

	it("declares the local-file pattern in Chrome's three-slash form only", () => {
		// Read as bytes, because the mistake is a spelling one: the two-slash forms
		// are a different, invalid pattern, and they are on the denylist beside the
		// pattern this extension ships.
		const raw = readFileSync(join(pkgRoot, "manifest.json"), "utf8");
		expect(raw).toContain('"file:///"');
		for (const pattern of DENIED_HOST_PATTERNS) {
			expect(raw, `denied pattern ${pattern} is in the manifest`).not.toContain(`"${pattern}"`);
		}
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
		// `'wasm-unsafe-eval'` is not `'unsafe-eval'`, and the distinction is
		// the whole point: it permits compiling WebAssembly and nothing else -
		// no string-to-code, no `eval`, no `Function`. MV3 provides it for
		// exactly this case, and SL-4.EXT.03 needs it to run the engine. The
		// ban below is on the quoted bare source, so a future edit cannot slip
		// `'unsafe-eval'` in beside it.
		expect(raw).not.toContain("'unsafe-eval'");
		expect(raw).not.toContain('"unsafe-eval"');
		expect(raw).not.toContain("unsafe-inline");
		// And the permission the engine does need is present, so removing it
		// (which would break every render) fails here too.
		expect(raw).toContain("'wasm-unsafe-eval'");
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

	it("justifies every optional host pattern in PERMISSIONS.md too", () => {
		// SL-4.EXT.09 extends the "one manifest, one record" rule to the optional
		// field. A pattern with no written justification is exactly the thing a
		// store reviewer rejects, and the reviewer reads the listing, not the
		// manifest — so the row has to exist before the pattern does.
		const doc = readFileSync(join(pkgRoot, "PERMISSIONS.md"), "utf8");
		for (const pattern of ALLOWED_OPTIONAL_HOST_PERMISSIONS) {
			expect(doc, `missing PERMISSIONS.md row for ${pattern}`).toContain(`\`${pattern}\``);
		}
		expect(doc).toContain("optional_host_permissions");
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

describe("the validator refuses a local-file pattern that has not been justified", () => {
	/** A manifest that passes every other check, so only the field under test fails. */
	const clean = {
		manifest_version: 3,
		permissions: [...ALLOWED_PERMISSIONS],
		host_permissions: [],
		background: { service_worker: "service-worker.js" },
		content_security_policy: {
			extension_pages: "script-src 'self' 'wasm-unsafe-eval'; object-src 'self';",
		},
	};

	it("passes when the optional list is exactly the approved one", () => {
		expect(
			validateManifest({ ...clean, optional_host_permissions: [...ALLOWED_OPTIONAL_HOST_PERMISSIONS] }),
		).toEqual([]);
	});

	it("fails on an optional pattern with no PERMISSIONS.md row", () => {
		const violations = validateManifest({
			...clean,
			optional_host_permissions: ["https://example.com/*"],
		});
		expect(violations.map((v) => v.field)).toContain("optional_host_permissions");
	});

	it("fails on a two-slash file pattern, which is a different and invalid pattern", () => {
		// The commonest mistake in this whole area, and one a reviewer would read
		// as a different and broader claim than the one justified.
		const violations = validateManifest({
			...clean,
			optional_host_permissions: ["file:///*"],
		});
		expect(violations.map((v) => v.field)).toContain("optional_host_permissions");
	});

	it("fails when the approved pattern is dropped from the manifest", () => {
		// Completeness, not just validity: the viewer's flow asks for this grant by
		// name, so a manifest without it ships a page offering a permission the
		// extension cannot hold.
		const violations = validateManifest({ ...clean, optional_host_permissions: [] });
		expect(violations.map((v) => v.field)).toContain("optional_host_permissions");
	});

	it("fails when the field is present but is not an array", () => {
		const violations = validateManifest({ ...clean, optional_host_permissions: "file:///" });
		expect(violations.map((v) => v.field)).toContain("optional_host_permissions");
	});
});
