/**
 * SL-4.EXT.04 — the bundled-only gate, proven falsifiable.
 *
 * A security gate that has only ever been observed to pass proves nothing. This
 * suite is built in two halves that must both hold:
 *
 * - **It fails.** Every violation class has a planted fixture: a real remote
 *   script URL, a real `eval`, a real CDN import, a CSP naming a remote host.
 *   Each is asserted to produce a named finding at a named line. If the scanner
 *   were neutered — an emptied regex, a `return []` short-circuit, an allowlist
 *   that swallowed everything — these fail immediately.
 * - **It stays quiet.** The other half is the false-positive suite, and it is
 *   not optional decoration. A gate that flags every `https://` in the tree is
 *   a gate that gets deleted on the first day it blocks a real commit. Every
 *   exemption the gate relies on is asserted here, including the ones that only
 *   fire on this repository's own files: the DNR `regexFilter` patterns, the
 *   URLs in `PATTERN_MATRIX`, the `blob:` scheme, CSP directive keywords and
 *   `<a href>`.
 *
 * The fourth describe closes the loop on the thing a synthetic fixture cannot
 * show: that the **real built package** scans clean *and* actually contains the
 * remote URLs the exemptions cover. A clean scan over a package with nothing in
 * it is the trivially-passing case this file exists to rule out.
 */

import { existsSync, readFileSync, readdirSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { describe, expect, it } from "vitest";
import { findCspViolations, findHtmlViolations, findManifestViolations } from "./bundle-docs.js";
import { DATA_ONLY_EXPORTS, maskJs } from "./bundle-js.js";
import {
	BUILD_WORKSPACE,
	type BundleFile,
	SHIPPED_FILES,
	classifyRedirectTarget,
	describeViolations,
	exemptRemoteLiterals,
	findRedirectTargetViolations,
	isBinaryAsset,
	scanBundle,
	shippedSet,
} from "./bundle-scan.js";

const pkgRoot = join(dirname(fileURLToPath(import.meta.url)), "..");
const distRoot = join(pkgRoot, "dist");

/**
 * A manifest that is clean by construction, so a fixture test that plants a
 * violation in it is testing that violation and nothing else.
 */
const CLEAN_MANIFEST = `${JSON.stringify(
	{
		manifest_version: 3,
		name: "Selis PDF Viewer",
		version: "0.0.1",
		description: "Local-first PDF viewer.",
		permissions: ["declarativeNetRequest", "offscreen"],
		host_permissions: [],
		background: { service_worker: "service-worker.js", type: "module" },
		content_security_policy: {
			extension_pages: "script-src 'self'; object-src 'self';",
		},
		action: { default_popup: "viewer.html" },
		icons: {},
		web_accessible_resources: [],
	},
	null,
	"\t",
)}\n`;

/** One clean packaged page, with a module reference that really resolves. */
const CLEAN_VIEWER_HTML = `<!doctype html>
<html lang="en">
	<head>
		<meta charset="utf-8" />
		<title>Selis PDF Viewer</title>
		<script type="module" src="./src/viewer-boot.js"></script>
	</head>
	<body><main id="selis-viewer"></main></body>
</html>
`;

/** Every file the ship list declares, each clean. */
function cleanPackage(): BundleFile[] {
	return [
		{ path: "manifest.json", text: CLEAN_MANIFEST },
		{ path: "viewer.html", text: CLEAN_VIEWER_HTML },
		{ path: "offscreen.html", text: '<!doctype html>\n<script src="offscreen.js"></script>\n' },
		{ path: "offscreen.js", text: 'globalThis.__selisOffscreen = "host";\n' },
		{
			path: "service-worker.js",
			text: 'import { buildRedirectRules } from "./src/permissions.js";\n',
		},
		{
			path: "src/permissions.js",
			text: 'export const VIEWER_PATH = "/viewer.html";\nexport function buildRedirectRules() {\n\treturn [];\n}\n',
		},
		{ path: "src/viewer-boot.js", text: "export const boot = () => undefined;\n" },
	];
}

/** The clean package with one file replaced. */
function withFile(path: string, text: string): BundleFile[] {
	return cleanPackage().map((file) => (file.path === path ? { path, text } : file));
}

/** The clean package with one file removed. */
function withoutFile(path: string): BundleFile[] {
	return cleanPackage().filter((file) => file.path !== path);
}

/** The clean package with one file added. */
function withExtraFile(path: string, text: string): BundleFile[] {
	return [...cleanPackage(), { path, text }];
}

/** The clean package with `manifest.json` built from a patched object. */
function withManifest(patch: Record<string, unknown>): BundleFile[] {
	const manifest = JSON.parse(CLEAN_MANIFEST) as Record<string, unknown>;
	return withFile("manifest.json", `${JSON.stringify({ ...manifest, ...patch }, null, "\t")}\n`);
}

/** The violation classes a scan produced, deduplicated. */
function classesOf(files: readonly BundleFile[]): string[] {
	return [...new Set(scanBundle(files).map((violation) => violation.class))].sort();
}

/** The full report text, for asserting that a failure names itself. */
function reportOf(files: readonly BundleFile[]): string {
	return describeViolations(scanBundle(files));
}

// ─────────────────────────────────────────────────────────────────────────────
// 1. The gate fails.
// ─────────────────────────────────────────────────────────────────────────────

describe("EXT.04 the gate rejects planted remote code", () => {
	it("passes the clean baseline (so every failure below is the planted defect)", () => {
		expect(reportOf(cleanPackage())).toBe(
			"clean: no remote code, no dynamic code, every reference resolves inside the package",
		);
	});

	it("rejects a remote <script src> in a packaged page", () => {
		const files = withFile(
			"viewer.html",
			'<script type="module" src="https://cdn.example.com/lib.js"></script>\n',
		);
		expect(classesOf(files)).toContain("remote-resource-ref");
		expect(reportOf(files)).toContain("cdn.example.com");
	});

	it("rejects a protocol-relative <script src>", () => {
		const files = withFile("viewer.html", '<script src="//cdn.example.com/lib.js"></script>\n');
		expect(classesOf(files)).toContain("remote-resource-ref");
	});

	it("rejects an unquoted remote <script src>", () => {
		// The form a quoted-attribute-only parser walks straight past.
		const files = withFile("viewer.html", "<script src=https://cdn.example.com/lib.js></script>\n");
		expect(classesOf(files)).toContain("remote-resource-ref");
	});

	it("rejects a remote stylesheet <link href>", () => {
		const files = withFile(
			"viewer.html",
			'<link rel="stylesheet" href="https://fonts.example.com/x.css">\n',
		);
		expect(classesOf(files)).toContain("remote-resource-ref");
	});

	it("rejects an inline <script>", () => {
		const files = withFile("viewer.html", "<script>globalThis.pwned = 1;</script>\n");
		expect(classesOf(files)).toContain("inline-script");
	});

	it("rejects a relative <script src> that resolves to nothing", () => {
		const files = withFile("viewer.html", '<script src="./src/not-shipped.js"></script>\n');
		expect(classesOf(files)).toContain("unresolved-local-ref");
	});

	it("rejects a remote URL literal in a shipped module", () => {
		const files = withFile(
			"src/viewer-boot.js",
			'export const ENGINE = "https://cdn.example.com/engine.js";\nexport const boot = 1;\n',
		);
		expect(classesOf(files)).toContain("remote-url-literal");
	});

	it("rejects a runtime fetch of a CDN in a shipped module", () => {
		const files = withFile(
			"src/viewer-boot.js",
			'export async function boot() {\n\tconst r = await fetch("https://cdn.example.com/e.wasm");\n\treturn r;\n}\n',
		);
		expect(classesOf(files)).toContain("remote-url-literal");
	});

	it("rejects a remote URL in the offscreen engine host", () => {
		const files = withFile(
			"offscreen.js",
			'const w = "https://cdn.example.com/worker.js";\nglobalThis.w = w;\n',
		);
		expect(classesOf(files)).toContain("remote-url-literal");
	});
});

describe("EXT.04 the gate rejects dynamic code execution", () => {
	/** A module whose only content is `body`; asserts the finding appears. */
	const rejects = (body: string): void => {
		const files = withFile("src/viewer-boot.js", body);
		expect(classesOf(files), `${body} -> ${reportOf(files)}`).toContain("dynamic-code");
	};

	it("rejects eval()", () => {
		rejects("export function boot(src) {\n\treturn eval(src);\n}\n");
	});

	it("rejects new Function()", () => {
		rejects('export const boot = () => new Function("return 1")();\n');
	});

	it("rejects Function() without new", () => {
		rejects('export const boot = () => Function("return 1")();\n');
	});

	it("rejects importScripts()", () => {
		rejects('export const boot = () => importScripts("./extra.js");\n');
	});

	it("rejects a string-bodied timer", () => {
		rejects('export const boot = () => setTimeout("globalThis.x = 1", 0);\n');
	});

	it("rejects document.write()", () => {
		rejects('export const boot = (el) => document.write("<b>x</b>");\n');
	});

	it("names eval on the line it is on", () => {
		const files = withFile(
			"src/viewer-boot.js",
			"export const a = 1;\nexport const b = 2;\neval(a);\n",
		);
		const finding = scanBundle(files).find((violation) => violation.class === "dynamic-code");
		expect(finding?.line).toBe(3);
		expect(finding?.file).toBe("src/viewer-boot.js");
	});
});

describe("EXT.04 the gate rejects module specifiers that leave the package", () => {
	it("rejects a bare CDN import", () => {
		const files = withFile(
			"src/viewer-boot.js",
			'import "https://cdn.example.com/lib.js";\nexport const boot = 1;\n',
		);
		expect(classesOf(files)).toContain("remote-import");
	});

	it("rejects a protocol-relative import", () => {
		const files = withFile("src/viewer-boot.js", 'import x from "//cdn.example.com/lib.js";\n');
		expect(classesOf(files)).toContain("remote-import");
	});

	it("rejects a relative import of a file that does not ship", () => {
		const files = withFile("src/viewer-boot.js", 'import { a } from "./missing.js";\n');
		expect(classesOf(files)).toContain("unresolved-import");
	});

	it("rejects a Node builtin import", () => {
		const files = withFile("src/viewer-boot.js", 'import { readFileSync } from "node:fs";\n');
		expect(classesOf(files)).toContain("node-builtin-import");
	});

	it("rejects a computed dynamic import it cannot resolve", () => {
		const files = withFile("src/viewer-boot.js", "export const boot = (n) => import(n);\n");
		expect(classesOf(files)).toContain("unresolvable-dynamic-import");
	});
});

describe("EXT.04 the gate rejects remote code in the manifest", () => {
	it("rejects a CDN content script", () => {
		const files = withManifest({
			content_scripts: [{ matches: ["<all_urls>"], js: ["https://cdn.example.com/x.js"] }],
		});
		expect(classesOf(files)).toContain("remote-resource-ref");
	});

	it("rejects a remote service worker", () => {
		const files = withManifest({
			background: { service_worker: "https://cdn.example.com/sw.js", type: "module" },
		});
		expect(classesOf(files)).toContain("remote-resource-ref");
	});

	it("rejects a default_popup that resolves to nothing", () => {
		const files = withManifest({ action: { default_popup: "popup.html" } });
		expect(classesOf(files)).toContain("unresolved-local-ref");
	});

	it("rejects an icon that is not packaged", () => {
		const files = withManifest({ icons: { 48: "icon48.png" } });
		expect(classesOf(files)).toContain("unresolved-local-ref");
	});

	it("rejects a remote URL in any other manifest string", () => {
		const files = withManifest({ description: "Powered by https://cdn.example.com/tracker" });
		expect(classesOf(files)).toContain("remote-manifest-url");
	});

	it("rejects a manifest that is not valid JSON", () => {
		const files = withFile("manifest.json", "{ not json");
		expect(classesOf(files)).toContain("invalid-manifest");
	});

	it("rejects a remote host in the extension-pages CSP", () => {
		const files = withManifest({
			content_security_policy: {
				extension_pages: "script-src 'self' https://cdn.example.com; object-src 'self';",
			},
		});
		expect(classesOf(files)).toContain("remote-csp-source");
	});

	it("rejects a protocol-relative host in the CSP", () => {
		const files = withManifest({
			content_security_policy: { extension_pages: "script-src 'self' //cdn.example.com;" },
		});
		expect(classesOf(files)).toContain("remote-csp-source");
	});

	it("rejects 'unsafe-eval' in the CSP", () => {
		const files = withManifest({
			content_security_policy: {
				extension_pages: "script-src 'self' 'unsafe-eval'; object-src 'self';",
			},
		});
		expect(classesOf(files)).toContain("loose-csp-source");
	});

	it("rejects 'unsafe-inline' in the CSP", () => {
		const files = withManifest({
			content_security_policy: {
				extension_pages: "script-src 'self' 'unsafe-inline'; object-src 'self';",
			},
		});
		expect(classesOf(files)).toContain("loose-csp-source");
	});

	it("rejects a relaxed CSP object instead of a pinned string", () => {
		const files = withManifest({ content_security_policy: { extension_pages: { a: 1 } } });
		expect(classesOf(files)).toContain("loose-csp-source");
	});
});

describe("EXT.04 the gate rejects a package that is not the declared one", () => {
	it("rejects a stray file sitting beside the shipped code", () => {
		const files = withExtraFile("src/permissions.test.js", "export const a = 1;\n");
		expect(classesOf(files)).toContain("unpackaged-file");
	});

	it("rejects a source map left in the package", () => {
		const files = withExtraFile("src/viewer-boot.js.map", '{"version":3}\n');
		expect(classesOf(files)).toContain("unpackaged-file");
	});

	it("rejects a package missing a file the ship list declares", () => {
		const files = withoutFile("viewer.html");
		const finding = scanBundle(files).find(
			(violation) => violation.class === "unresolved-local-ref",
		);
		expect(finding?.file).toBe("viewer.html");
		expect(finding?.detail).toContain("missing from the built package");
	});

	it("rejects a remote URL in a packaged file that is no module, page or manifest", () => {
		const files = withExtraFile("assets/notes.txt", "source: https://cdn.example.com/a.js\n");
		expect(classesOf(files)).toContain("remote-url-literal");
	});

	it("rejects a DNR redirect whose target is not a packaged file", () => {
		const shipped = shippedSet();
		expect(classifyRedirectTarget("/viewer.html", shipped)).toBeNull();
		expect(classifyRedirectTarget("viewer.html", shipped)).toBeNull();
		expect(classifyRedirectTarget("https://cdn.example.com/v.html", shipped)).toMatch(/remote/);
		expect(classifyRedirectTarget("//cdn.example.com/v.html", shipped)).toMatch(/remote/);
		expect(classifyRedirectTarget("/not-shipped.html", shipped)).toMatch(/not in the package/);
	});
});

// ─────────────────────────────────────────────────────────────────────────────
// 2. The gate stays quiet. Every exemption it depends on is pinned here.
// ─────────────────────────────────────────────────────────────────────────────

describe("EXT.04 the gate does not flag what is not remote code", () => {
	it("accepts a packaged module <script> — the DoD's real viewer page", () => {
		// This is the exact shape of `viewer.html` as shipped. The first version
		// of the gate read the attribute list as a flat array of whole matches,
		// never saw the name `src`, and called every packaged script inline.
		expect(classesOf(cleanPackage())).not.toContain("inline-script");
	});

	it("accepts an <a href> the user has to click", () => {
		const files = withFile(
			"viewer.html",
			'<a href="https://selis.example/help">Help</a>\n<script src="./src/viewer-boot.js"></script>\n',
		);
		expect(scanBundle(files)).toEqual([]);
	});

	it("accepts a remote URL in a JS comment", () => {
		const files = withFile(
			"src/viewer-boot.js",
			"// never load https://cdn.example.com/lib.js - ADR-P0028\n/* also not https://cdn.example.com/x.js */\nexport const boot = 1;\n",
		);
		expect(scanBundle(files)).toEqual([]);
	});

	it("accepts a remote URL in an HTML comment", () => {
		const files = withFile(
			"viewer.html",
			'<!-- the old build pulled https://cdn.example.com/lib.js; it must not -->\n<script src="./src/viewer-boot.js"></script>\n',
		);
		expect(scanBundle(files)).toEqual([]);
	});

	it("accepts the word eval in a comment and in a string", () => {
		const files = withFile(
			"src/viewer-boot.js",
			'// eval() is banned here\nexport const BAN = "do not call eval( on anything";\nexport const boot = 1;\n',
		);
		expect(scanBundle(files)).toEqual([]);
	});

	it("accepts the word importScripts in a comment", () => {
		const files = withFile(
			"service-worker.js",
			'// importScripts() would be remote code; the ruleset is imported instead\nimport { buildRedirectRules } from "./src/permissions.js";\n',
		);
		expect(scanBundle(files)).toEqual([]);
	});

	it("accepts a DNR regexFilter, the most remote-looking string we ship", () => {
		const files = withFile(
			"src/permissions.js",
			'export function buildRedirectRules() {\n\treturn [{ condition: { regexFilter: "^https?://[^?#\\\\s]+(?i:\\\\.pdf)(?:[?#]\\\\S*)?$" } }];\n}\n',
		);
		expect(scanBundle(files)).toEqual([]);
	});

	it("accepts a data-only export of URLs the ruleset must recognise", () => {
		const files = withFile(
			"src/permissions.js",
			'export const PATTERN_MATRIX = [\n\t{ url: "https://example.com/a.pdf" },\n\t{ url: "https://example.org/b.pdf?x=1" },\n];\nexport function buildRedirectRules() {\n\treturn [];\n}\n',
		);
		expect(scanBundle(files)).toEqual([]);
	});

	it("stops honouring a data-only export the moment it calls something", () => {
		// The allowlist is structural, not a name. A call in the initializer
		// means the value is computed, and a computed value is not reviewable
		// as data.
		const files = withFile(
			"src/permissions.js",
			'const base = "https://example.com";\nexport const PATTERN_MATRIX = [join(base, "a.pdf")];\nexport function buildRedirectRules() {\n\treturn [];\n}\n',
		);
		expect(classesOf(files)).toContain("remote-url-literal");
	});

	it("accepts a blob: URL, which names an origin but is made locally", () => {
		const files = withFile(
			"src/viewer-boot.js",
			'export const url = URL.createObjectURL(new Blob([]));\nexport const sample = "blob:https://example.com/550e8400-e29b-41d4-a716-446655440000";\n',
		);
		expect(scanBundle(files)).toEqual([]);
	});

	it("accepts a data: URL in a page", () => {
		const files = withFile("viewer.html", '<img src="data:image/png;base64,iVBORw0KGgo=" />\n');
		expect(scanBundle(files)).toEqual([]);
	});

	it("accepts a data- attribute, which the browser never fetches", () => {
		const files = withFile("viewer.html", '<div data-src="https://cdn.example.com/x.js"></div>\n');
		expect(scanBundle(files)).toEqual([]);
	});

	it("accepts a data: URL in a module", () => {
		const files = withFile(
			"src/viewer-boot.js",
			'export const FONT = "data:font/woff2;base64,d09GMg==";\nexport const boot = FONT;\n',
		);
		expect(scanBundle(files)).toEqual([]);
	});

	it("accepts store-listing links and permission patterns in the manifest", () => {
		const files = withManifest({
			homepage_url: "https://selis.example",
			support_url: "https://selis.example/support",
			host_permissions: ["https://example.com/*"],
			permissions: ["declarativeNetRequest", "offscreen", "storage"],
		});
		expect(scanBundle(files)).toEqual([]);
	});

	it("accepts CSP directive keywords that are not hosts", () => {
		// The `apps/web/host` precedent: `upgrade-insecure-requests` upgrades
		// http to https and fetches nothing.
		const files = withManifest({
			content_security_policy: {
				extension_pages:
					"script-src 'self'; object-src 'self'; upgrade-insecure-requests; report-to selis",
			},
		});
		expect(scanBundle(files)).toEqual([]);
	});

	it("accepts 'wasm-unsafe-eval', which the packaged engine will need", () => {
		const files = withManifest({
			content_security_policy: {
				extension_pages: "script-src 'self' 'wasm-unsafe-eval'; object-src 'self';",
			},
		});
		expect(scanBundle(files)).toEqual([]);
	});

	it("still reports a host in a CSP that also carries a directive keyword", () => {
		// The keyword exemption must not become a hole: the host is still there.
		const files = withManifest({
			content_security_policy: {
				extension_pages: "script-src 'self' https://cdn.example.com; upgrade-insecure-requests;",
			},
		});
		expect(classesOf(files)).toContain("remote-csp-source");
	});

	it("accepts a relative import that resolves inside the package", () => {
		const files = withFile(
			"src/viewer-boot.js",
			'import { VIEWER_PATH } from "./permissions.js";\nexport const boot = VIEWER_PATH;\n',
		);
		expect(scanBundle(files)).toEqual([]);
	});

	it("does not treat the build workspace as part of the package", () => {
		// `dist/pkg/` holds the raw tsc output, including the gate's own tests.
		// It is excluded by name so tests can never reach the store upload.
		expect(BUILD_WORKSPACE).toBe("pkg");
		const files = withExtraFile(`${BUILD_WORKSPACE}/manifest.test.js`, "export const a = 1;\n");
		expect(classesOf(files)).not.toContain("unpackaged-file");
	});

	it("treats a NUL-bearing asset as binary rather than scanning its bytes", () => {
		// The WASM payload SL-4.EXT.05 will add: data in the package by
		// construction, and utf8-decoding it would be noise.
		expect(isBinaryAsset(" asm  ")).toBe(true);
		expect(isBinaryAsset("export const a = 1;\n")).toBe(false);
		// Not on the ship list yet, so it is an `unpackaged-file` — but its
		// bytes are never read as text, which is the point of the skip.
		const files = withExtraFile("src/engine.wasm", " asm   binary");
		expect(classesOf(files)).toEqual(["unpackaged-file"]);
	});
});

// ─────────────────────────────────────────────────────────────────────────────
// 3. The mask cannot be walked past.
// ─────────────────────────────────────────────────────────────────────────────

describe("EXT.04 the lexical mask is not evadable", () => {
	it("still sees an eval() planted after a regex literal containing a quote", () => {
		// The first version of the stripper skipped strings by scanning to the
		// next matching quote. `const q = /'/;` therefore swallowed the rest of
		// the file and this eval() was invisible. If this test ever goes green
		// for the wrong reason, the mask has regressed.
		const files = withFile(
			"src/viewer-boot.js",
			'const q = /\';\nexport const boot = () => {\n\teval("payload");\n};\n',
		);
		expect(classesOf(files)).toContain("dynamic-code");
	});

	it("still sees a new Function() after a regex literal containing a quote", () => {
		const files = withFile(
			"src/viewer-boot.js",
			'const q = /"/;\nexport const boot = () => new Function("x");\n',
		);
		expect(classesOf(files)).toContain("dynamic-code");
	});

	it("still sees a remote URL after a regex literal containing a quote", () => {
		const files = withFile(
			"src/viewer-boot.js",
			'const q = /\';\nexport const ENGINE = "https://cdn.example.com/e.js";\n',
		);
		expect(classesOf(files)).toContain("remote-url-literal");
	});

	it("does not mistake division for a regex and blank the rest of the file", () => {
		// The other direction of the same heuristic: if `/` were always taken as
		// a regex opener, this file's tail would vanish from the scan.
		const files = withFile(
			"src/viewer-boot.js",
			"export const half = (total) => {\n\tconst half = total / 2;\n\tconst ratio = half / total;\n\teval(half);\n\treturn ratio;\n};\n",
		);
		expect(classesOf(files)).toContain("dynamic-code");
	});

	it("does not treat a division result as a string body", () => {
		const files = withFile(
			"src/viewer-boot.js",
			'export const q = (a) => a / 2;\nexport const ENGINE = "https://cdn.example.com/e.js";\n',
		);
		expect(classesOf(files)).toContain("remote-url-literal");
	});

	it("keeps both masks the same length as the source", () => {
		const source = 'const a = "x";\n// c\nconst r = /y/g;\nconst t = `z${a}`;\n';
		const mask = maskJs(source);
		expect(mask.text).toHaveLength(source.length);
		expect(mask.code).toHaveLength(source.length);
	});

	it("blanks a string body in code but keeps the quotes", () => {
		const mask = maskJs('const a = "eval(";');
		expect(mask.code).not.toContain("eval(");
		expect(mask.code).toContain('"');
		expect(mask.text).toContain("eval(");
	});
});

// ─────────────────────────────────────────────────────────────────────────────
// 4. The real, built package. The last half of the falsifiability argument.
// ─────────────────────────────────────────────────────────────────────────────

/** Every file in `dir`, as the CLI sees them (the build workspace excluded). */
function readBuiltPackage(dir: string): BundleFile[] {
	const found: BundleFile[] = [];
	const walk = (current: string, prefix: string): void => {
		for (const entry of readdirSync(current, { withFileTypes: true })) {
			const path = prefix === "" ? entry.name : `${prefix}/${entry.name}`;
			if (entry.isDirectory()) {
				if (path === BUILD_WORKSPACE) {
					continue;
				}
				walk(join(current, entry.name), path);
				continue;
			}
			found.push({ path, text: readFileSync(join(current, entry.name), "utf8") });
		}
	};
	walk(dir, "");
	return found;
}

// The built package only exists after `pnpm --filter @selis/extension build`,
// which `pnpm test` runs first. Asserting the prerequisite rather than skipping
// keeps this from decaying into a test that passes because it did nothing.
describe("EXT.04 the real built package", () => {
	it("was built (run `pnpm --filter @selis/extension build` first)", () => {
		expect(
			existsSync(join(distRoot, "manifest.json")),
			`no built package at ${distRoot} - the gate never passes on a missing artefact`,
		).toBe(true);
	});

	it("scans clean", () => {
		const files = readBuiltPackage(distRoot);
		expect(files.length).toBeGreaterThan(0);
		expect(describeViolations(scanBundle(files))).toBe(
			"clean: no remote code, no dynamic code, every reference resolves inside the package",
		);
	});

	it("contains every file the ship list declares, and nothing else", () => {
		const present = readBuiltPackage(distRoot)
			.map((file) => file.path)
			.sort();
		expect(present).toEqual([...SHIPPED_FILES].sort());
	});

	it("really does contain the remote URLs the exemptions cover", () => {
		// Without this, "scans clean" is the trivially-passing case: a package
		// with no URLs in it would pass a gate that never looks.
		const built = readBuiltPackage(distRoot).filter((file) => file.path === "src/permissions.js");
		const exempt = built.flatMap((file) => exemptRemoteLiterals(file));
		expect(
			exempt.length,
			"PATTERN_MATRIX / DENIED_HOST_PATTERNS should carry many URLs",
		).toBeGreaterThan(20);
		for (const literal of exempt) {
			expect(DATA_ONLY_EXPORTS.has(literal.exemptBy)).toBe(true);
		}
		// And the same URLs, planted outside the reviewed export, must fail.
		const smuggled = withFile("src/permissions.js", `export const X = "${exempt[0]?.value}";\n`);
		expect(classesOf(smuggled)).toContain("remote-url-literal");
	});

	it("fails when a remote script is planted in the built viewer page", () => {
		// The end-to-end falsifiability check on the real artefact: not a
		// synthetic fixture, the page Chrome would actually load.
		const planted = readBuiltPackage(distRoot).map((file) =>
			file.path === "viewer.html"
				? {
						path: file.path,
						text: `${file.text}\n<script src="https://cdn.example.com/remote.js"></script>\n`,
					}
				: file,
		);
		const violations = scanBundle(planted);
		expect(violations.map((violation) => violation.class)).toContain("remote-resource-ref");
		expect(describeViolations(violations)).toContain("cdn.example.com/remote.js");
	});

	it("derives the DNR redirect check from the live ruleset, and it passes", () => {
		expect(findRedirectTargetViolations()).toEqual([]);
	});
});

// ─────────────────────────────────────────────────────────────────────────────
// 5. The pieces the aggregation is built from, exercised directly.
// ─────────────────────────────────────────────────────────────────────────────

describe("EXT.04 the document layer", () => {
	const shipped = shippedSet(SHIPPED_FILES);

	it("classifies references by where they resolve", () => {
		expect(findManifestViolations(CLEAN_MANIFEST, shipped)).toEqual([]);
		expect(findHtmlViolations("viewer.html", CLEAN_VIEWER_HTML, shipped)).toEqual([]);
	});

	it("reports a remote host in script-src once per distinct problem", () => {
		// Two findings, not one and not three: the host is remote, and it is
		// also a `script-src` source outside the allowlist. The generic
		// manifest URL check must not add a third for the same string.
		const csp = "script-src 'self' https://cdn.example.com;";
		expect(
			findCspViolations(csp)
				.map((violation) => violation.class)
				.sort(),
		).toEqual(["loose-csp-source", "remote-csp-source"]);
		const manifest = JSON.stringify({
			manifest_version: 3,
			content_security_policy: { extension_pages: csp },
		});
		expect(findManifestViolations(manifest, shippedSet(SHIPPED_FILES))).toHaveLength(2);
	});
});
