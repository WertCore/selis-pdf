/**
 * SL-4.EXT.04 - the document layer: the manifest and the packaged pages.
 *
 * `manifest.json` is what Chrome reads first and what a store reviewer opens
 * first, so every string leaf in it is classified: a packaged path, an exempt
 * link, or a build failure. The HTML pages get the same treatment for the
 * attributes the browser actually fetches.
 *
 * The CSP handling copies `apps/web/host`: `headers.test.ts` there removes the
 * one directive keyword that legitimately contains a scheme before asserting
 * that the policy names no remote host. Same false positive, same fix.
 */

import { classifyRef, lineOf } from "./bundle-paths.js";

/** One finding in a packaged document. */
export interface DocViolation {
	readonly line: number;
	readonly class:
		| "remote-resource-ref"
		| "unresolved-local-ref"
		| "inline-script"
		| "remote-csp-source"
		| "loose-csp-source"
		| "remote-manifest-url"
		| "invalid-manifest";
	readonly detail: string;
}

/** A remote URL anywhere in a value. */
const REMOTE_URL = /(?:https?|wss?|ftps?):\/\//;

/** HTML attributes the browser *fetches*. `<a href>` is deliberately absent. */
const FETCHING_ATTRIBUTES: readonly string[] = [
	"src",
	"srcset",
	"data",
	"poster",
	"action",
	"formaction",
];

/** Tags whose `href` is a fetched resource rather than a navigation. */
const HREF_FETCHING_TAGS: ReadonlySet<string> = new Set(["link", "use", "image"]);

/** CSP directive keywords that name a scheme or a report endpoint, not a host. */
const CSP_DIRECTIVE_KEYWORDS: readonly string[] = [
	"upgrade-insecure-requests",
	"block-all-mixed-content",
	"report-to",
];

/**
 * `script-src` sources that do not grant general script execution.
 *
 * `'wasm-unsafe-eval'` is in the set on purpose: instantiating the packaged
 * WASM (SL-4.EXT.05) needs compile permission, and it is not eval of a string.
 */
const ALLOWED_SCRIPT_SOURCES: ReadonlySet<string> = new Set([
	"'self'",
	"'wasm-unsafe-eval'",
	"'none'",
]);

/**
 * Remote hosts and script sources in one CSP string.
 *
 * The directive keywords are removed first: `upgrade-insecure-requests`
 * upgrades http to https, it does not fetch anything, and `apps/web/host`
 * handles the same false positive for the web CSP. The exemption is narrow by
 * construction - a *host* in any other position still fails.
 */
export function findCspViolations(csp: string): DocViolation[] {
	const violations: DocViolation[] = [];
	let withoutKeywords = csp;
	for (const keyword of CSP_DIRECTIVE_KEYWORDS) {
		withoutKeywords = withoutKeywords.replaceAll(keyword, "");
	}
	if (REMOTE_URL.test(withoutKeywords) || /(^|[\s;])\/\//.test(withoutKeywords)) {
		violations.push({
			line: 0,
			class: "remote-csp-source",
			detail: `CSP names a remote host: ${csp}`,
		});
	}
	for (const directive of csp.split(";")) {
		const parts = directive
			.trim()
			.split(/\s+/)
			.filter((part) => part.length > 0);
		const name = parts[0] ?? "";
		if (!name.startsWith("script-src")) {
			continue;
		}
		for (const source of parts.slice(1)) {
			if (!ALLOWED_SCRIPT_SOURCES.has(source)) {
				violations.push({
					line: 0,
					class: "loose-csp-source",
					detail: `script-src allows '${source}'; only 'self' and 'wasm-unsafe-eval' are permitted (ADR-P0028)`,
				});
			}
		}
	}
	return violations;
}

/**
 * An attribute, with its value: `name="v"`, `name='v'` or an unquoted `name=v`.
 *
 * The unquoted form is accepted because HTML allows it and a gate that only
 * understood the quoted forms could be walked straight past with
 * `<script src=https://cdn.example/x.js>`. The unquoted value stops at any
 * character that cannot be part of one, so `disabled` and `src=./a.js>` both
 * parse the way a browser reads them.
 */
const ATTRIBUTE = /([\w:-]+)(?:\s*=\s*(?:"([^"]*)"|'([^']*)'|([^\s"'=<>`]+)))?/g;

/**
 * Remote or unresolvable resource references, and inline scripts, in one page.
 *
 * `<a href>` is not checked: a link the user clicks is not remote code. Every
 * attribute the browser fetches on its own is, and a relative one has to point
 * at a file that actually ships.
 */
export function findHtmlViolations(
	filePath: string,
	html: string,
	shipped: ReadonlySet<string>,
): DocViolation[] {
	const violations: DocViolation[] = [];
	// Comments are blanked (length preserved) so a documented example URL in an
	// HTML comment is prose, exactly as it is in a JS comment.
	const text = html.replace(/<!--[\s\S]*?-->/g, (comment) => comment.replace(/[^\n]/g, " "));
	const tags = /<([a-zA-Z][\w-]*)((?:[^>"']|"[^"]*"|'[^']*')*)>/g;
	for (const tag of text.matchAll(tags)) {
		const name = (tag[1] ?? "").toLowerCase();
		const attributes = tag[2] ?? "";
		const line = lineOf(text, tag.index ?? 0);
		const parsed: { name: string; value: string }[] = [];
		for (const attribute of attributes.matchAll(ATTRIBUTE)) {
			parsed.push({
				name: (attribute[1] ?? "").toLowerCase(),
				value: attribute[2] ?? attribute[3] ?? attribute[4] ?? "",
			});
		}
		if (name === "script" && !parsed.some((attribute) => attribute.name === "src")) {
			violations.push({
				line,
				class: "inline-script",
				detail: "inline <script> - MV3 extension pages may only load packaged scripts",
			});
		}
		for (const { name: attribute, value } of parsed) {
			const fetching =
				FETCHING_ATTRIBUTES.includes(attribute) ||
				(attribute === "href" && HREF_FETCHING_TAGS.has(name));
			if (!fetching || value.trim().length === 0) {
				continue;
			}
			const resolved = classifyRef(filePath, value, shipped);
			if (resolved.kind === "remote") {
				violations.push({
					line,
					class: "remote-resource-ref",
					detail: `<${name} ${attribute}> fetches '${value}' - package the resource instead`,
				});
			} else if (resolved.kind === "unresolved") {
				violations.push({
					line,
					class: "unresolved-local-ref",
					detail: `<${name} ${attribute}> references '${value}', which is not in the package`,
				});
			}
		}
	}
	return violations;
}
/**
 * Manifest fields whose value is a *link, not code*.
 *
 * Chrome never loads these as executable content: they are store-listing
 * metadata, permission patterns (the EXT.01 gate owns those), or a search
 * provider that is meant to be a remote query URL.
 *
 * Deliberately *not* exempt: `name`, `short_name`, `description`, `version`,
 * `minimum_chrome_version`. An earlier version exempted them "to be safe", which
 * only meant a URL hidden in the description — the one field no reviewer
 * parses — passed unremarked. A URL is never legitimate in any of them, so
 * there is nothing to exempt.
 */
const EXEMPT_MANIFEST_FIELDS: ReadonlySet<string> = new Set([
	"content_security_policy",
	"host_permissions",
	"optional_host_permissions",
	"permissions",
	"optional_permissions",
	"homepage_url",
	"support_url",
	"bugs",
	"externally_connectable",
	"search_provider",
]);

/** Manifest leaf keys that hold a CSP, and are judged by `findCspViolations`. */
const CSP_LEAF_KEYS: ReadonlySet<string> = new Set(["extension_pages", "sandbox"]);

/** Manifest leaf keys whose value always names a file inside the package. */
const PACKAGE_PATH_FIELDS: ReadonlySet<string> = new Set([
	"service_worker",
	"page",
	"default_popup",
	"options_page",
	"devtools_page",
	"default_path",
	"js",
	"css",
	"resources",
	"pages",
]);

/** Walk every string leaf of a parsed manifest, with its dotted field path. */
function* manifestStrings(
	node: unknown,
	prefix: readonly string[] = [],
): Generator<{ readonly path: string; readonly value: string }> {
	if (typeof node === "string") {
		yield { path: prefix.join("."), value: node };
		return;
	}
	if (Array.isArray(node)) {
		for (const [index, entry] of node.entries()) {
			yield* manifestStrings(entry, [...prefix, String(index)]);
		}
		return;
	}
	if (node !== null && typeof node === "object") {
		for (const [key, value] of Object.entries(node as Record<string, unknown>)) {
			yield* manifestStrings(value, [...prefix, key]);
		}
	}
}

/**
 * `manifest.json` checks: every string leaf is a packaged path, an exempt link,
 * or a violation.
 *
 * Deliberately redundant with the HTML check, because the manifest is the file
 * that decides what the browser loads: a `content_scripts[].js` on a CDN, an
 * `icons` key that is a URL, or a `default_popup` that resolves to nothing all
 * fail here by name.
 */
export function findManifestViolations(
	manifestText: string,
	shipped: ReadonlySet<string>,
): DocViolation[] {
	let parsed: unknown;
	try {
		parsed = JSON.parse(manifestText);
	} catch (error) {
		return [
			{
				line: 0,
				class: "invalid-manifest",
				detail: `manifest.json is not valid JSON: ${String(error)}`,
			},
		];
	}
	const violations: DocViolation[] = [];
	const lineOfValue = (value: string): number => lineOf(manifestText, manifestText.indexOf(value));
	for (const leaf of manifestStrings(parsed)) {
		const segments = leaf.path.split(".");
		// The field a leaf *belongs to*, with array indices dropped:
		// `content_scripts.0.js.0` is a `js` field, not a `0` field. Without
		// this, every value inside a manifest array skipped the packaged-path
		// check entirely and a CDN `content_scripts[].js` slipped through.
		const key = segments.filter((segment) => !/^\d+$/.test(segment)).at(-1) ?? "";
		if (EXEMPT_MANIFEST_FIELDS.has(key)) {
			continue;
		}
		if (PACKAGE_PATH_FIELDS.has(key)) {
			const resolved = classifyRef("manifest.json", leaf.value, shipped);
			if (resolved.kind === "remote") {
				violations.push({
					line: lineOfValue(leaf.value),
					class: "remote-resource-ref",
					detail: `${leaf.path} points at '${leaf.value}' - package the file instead`,
				});
			} else if (resolved.kind === "unresolved") {
				violations.push({
					line: lineOfValue(leaf.value),
					class: "unresolved-local-ref",
					detail: `${leaf.path} references '${leaf.value}', which is not in the package`,
				});
			}
			continue;
		}
		if (CSP_LEAF_KEYS.has(key)) {
			// Checked by findCspViolations below, which understands directive
			// keywords. Running the blunt URL check over the same string as well
			// would report one mistake twice.
			continue;
		}
		if (REMOTE_URL.test(leaf.value) || leaf.value.trim().startsWith("//")) {
			violations.push({
				line: lineOfValue(leaf.value),
				class: "remote-manifest-url",
				detail: `${leaf.path} contains the remote URL '${leaf.value}'`,
			});
		}
	}
	const icons = (parsed as { icons?: Record<string, unknown> }).icons ?? {};
	for (const key of Object.keys(icons)) {
		if (classifyRef("manifest.json", key, shipped).kind !== "packaged") {
			violations.push({
				line: 0,
				class: "unresolved-local-ref",
				detail: `icons['${key}'] is not a packaged file`,
			});
		}
	}
	const csp = (parsed as { content_security_policy?: { extension_pages?: unknown } })
		.content_security_policy?.extension_pages;
	if (typeof csp === "string") {
		violations.push(...findCspViolations(csp));
	} else if (
		(parsed as { content_security_policy?: unknown }).content_security_policy !== undefined
	) {
		violations.push({
			line: 0,
			class: "loose-csp-source",
			detail: "content_security_policy.extension_pages must be a pinned CSP string (ADR-P0028)",
		});
	}
	return violations;
}
