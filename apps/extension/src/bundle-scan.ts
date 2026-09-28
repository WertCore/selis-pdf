/**
 * SL-4.EXT.04 - the bundled-only build gate (ADR-P0028).
 *
 * The rule, in one sentence: **every byte the browser executes ships inside the
 * package, and nothing in the package points at a remote origin.** Chrome Web
 * Store policy forbids remotely-hosted code in MV3 and punishes anything that
 * resembles it, so the check is mechanical rather than a review habit:
 * {@link scanBundle} runs over the **built** package and `bundle-check.ts`
 * exits non-zero on any finding. `BUNDLING.md` is the written policy; this
 * module is the enforcement, split into three layers:
 *
 * - {@link bundle-paths} - what ships, and how a reference resolves.
 * - {@link bundle-js} / {@link bundle-docs} - what a shipped module, page and
 *   manifest are allowed to say.
 * - this file - the aggregation, the packaging rule, and the report.
 *
 * ## Which strings are violations, and which are not
 *
 * Violations: a remote or protocol-relative URL in any resource position (an
 * HTML `src`, a `<link href>`, a manifest field naming a packaged file, any
 * other manifest string); a remote host in a CSP directive; a `script-src`
 * source other than `'self'` / `'wasm-unsafe-eval'`; a remote URL literal in
 * shipped code; `eval` / `new Function` / `Function("…")` / `importScripts` / a
 * string-bodied timer / `document.write`; a module specifier that is remote, is
 * a `node:` builtin, is computed, or does not resolve to a packaged file; a DNR
 * redirect whose target is not a packaged file; a file in the package that is
 * not on the ship list.
 *
 * Exemptions, each with a reason: comments (a JSDoc example is prose); the
 * `upgrade-insecure-requests` CSP keyword (a directive, not a host - the
 * `apps/web/host` precedent); a DNR `regexFilter` (a match pattern, never
 * fetched); the two reviewed data-only exports {@link DATA_ONLY_EXPORTS}
 * (remote URLs the ruleset must *recognise*), and only while their initializer
 * is pure data, so the exemption cannot smuggle a call; `<a href>` (a link the
 * user clicks); store-listing links and permission patterns (Chrome never loads
 * them as code); `blob:` / `data:` (local by construction); binary assets.
 *
 * The module is pure - no `fs`, no `chrome.*` - so the gate's own behaviour is
 * testable, and a neutered scanner fails CI instead of shipping remote code.
 * `bundle.test.ts` is what makes that claim checkable: it plants a violation of
 * every class above, and it asserts the exempt count on the real built package
 * so that "clean" cannot mean "looked at nothing".
 */

import { type DocViolation, findHtmlViolations, findManifestViolations } from "./bundle-docs.js";
import {
	DATA_ONLY_EXPORTS,
	type JsViolation,
	findModuleViolations,
	maskJs,
	remoteLiteralsInModule,
} from "./bundle-js.js";
import {
	BUILD_WORKSPACE,
	SHIPPED_FILES,
	lineOf,
	normalisePackagePath,
	shippedSet,
} from "./bundle-paths.js";
import { buildRedirectRules } from "./permissions.js";

export {
	BUILD_WORKSPACE,
	OWN_BUILD_TREE,
	PACKAGE_ENTRIES,
	SHARED_BUILD_TREE,
	SHIPPED_FILES,
	buildSourceOf,
	shippedSet,
} from "./bundle-paths.js";
export { DATA_ONLY_EXPORTS } from "./bundle-js.js";
export type { PackageEntry, RefClass } from "./bundle-paths.js";

/** Every way the gate can fail. Each is a real Chrome Web Store rejection risk. */
export type ViolationClass =
	| DocViolation["class"]
	| JsViolation["class"]
	/** A DNR redirect whose target is not a packaged file. */
	| "remote-redirect-target"
	/** A file in the package that is not on the ship list. */
	| "unpackaged-file";

/** One gate failure: what, where, and why it matters. */
export interface BundleViolation {
	readonly file: string;
	/** 1-based line, or 0 when the finding is about the file as a whole. */
	readonly line: number;
	readonly class: ViolationClass;
	readonly detail: string;
}

/** One file of the built package, as handed to the gate. */
export interface BundleFile {
	/** Path inside the package (`/`-separated, relative to `dist/`). */
	readonly path: string;
	readonly text: string;
}

/** A remote URL anywhere in a value. */
const REMOTE_URL = /(?:https?|wss?|ftps?):\/\//;

/**
 * Files in the package directory that are not on the ship list.
 *
 * This is what makes {@link SHIPPED_FILES} load-bearing: a stray file - a test
 * bundle, a source map, a second copy of the engine - cannot reach the store
 * just by being next to the code. The raw `tsc` output directory
 * ({@link BUILD_WORKSPACE}) is the one exclusion, and it is excluded by name.
 */
export function findUnpackagedFiles(paths: readonly string[]): BundleViolation[] {
	const shipped = shippedSet();
	const violations: BundleViolation[] = [];
	for (const path of paths) {
		const normalised = normalisePackagePath(path);
		if (normalised === "" || shipped.has(normalised)) {
			continue;
		}
		if (normalised === BUILD_WORKSPACE || normalised.startsWith(`${BUILD_WORKSPACE}/`)) {
			continue;
		}
		violations.push({
			file: normalised,
			line: 0,
			class: "unpackaged-file",
			detail: `'${normalised}' is in the package but not on SHIPPED_FILES - add a PACKAGE_ENTRIES row or delete it`,
		});
	}
	return violations;
}

/**
 * DNR redirect targets must be packaged files.
 *
 * A redirect to a remote `extensionPath` is remote code with extra steps: the
 * browser performs the navigation, so nothing in the module graph would show
 * it. Re-derived from the live ruleset rather than a copy, so a rule edit is
 * covered without touching the gate.
 */
export function findRedirectTargetViolations(): BundleViolation[] {
	const shipped = shippedSet();
	const violations: BundleViolation[] = [];
	for (const rule of buildRedirectRules()) {
		const target = rule.action.redirect.extensionPath;
		const resolved = classifyRedirectTarget(target, shipped);
		if (resolved === null) {
			continue;
		}
		violations.push({
			file: "src/permissions.js",
			line: 0,
			class: "remote-redirect-target",
			detail: `DNR rule ${rule.id} redirects to '${target}': ${resolved}`,
		});
	}
	return violations;
}

/** `null` when `target` is packaged, else why it is not. */
export function classifyRedirectTarget(
	target: string,
	shipped: ReadonlySet<string>,
): string | null {
	const trimmed = target.trim();
	if (trimmed.startsWith("//") || /^[a-zA-Z][a-zA-Z0-9+.-]*:/.test(trimmed)) {
		return "a remote target is not a packaged file (ADR-P0028)";
	}
	// DNR `extensionPath` is package-relative; a leading `/` is the same root.
	const resolved = normalisePackagePath(trimmed);
	return shipped.has(resolved) ? null : `'${resolved}' is not in the package`;
}

/** Whether `text` looks binary (a NUL byte early in the file). */
export function isBinaryAsset(text: string): boolean {
	const limit = Math.min(text.length, 8192);
	for (let i = 0; i < limit; i += 1) {
		if (text.charCodeAt(i) === 0) {
			return true;
		}
	}
	return false;
}
/**
 * The gate: scan a whole built package and return every violation.
 *
 * `files` is the *built* package, not the sources - the point of SL-4.EXT.04 is
 * that what the browser loads is what gets checked. Structural findings come
 * first (a file that should not ship, a row whose file is missing), then the
 * modules, then the documents that point into them.
 */
export function scanBundle(files: readonly BundleFile[]): BundleViolation[] {
	const shipped = shippedSet();
	const present = new Set(files.map((file) => file.path));
	const violations: BundleViolation[] = [];
	for (const path of present) {
		violations.push(...findUnpackagedFiles([path]));
	}
	for (const path of SHIPPED_FILES) {
		if (!present.has(path)) {
			violations.push({
				file: path,
				line: 0,
				class: "unresolved-local-ref",
				detail: `${path} is on SHIPPED_FILES but missing from the built package`,
			});
		}
	}
	for (const file of files) {
		if (isBinaryAsset(file.text)) {
			// A WASM payload (SL-4.EXT.05) is data, not a URL to fetch: its bytes
			// are in the package by construction. Skipped here, and the report
			// says so, rather than hiding that anything was skipped.
			continue;
		}
		if (file.path.endsWith(".js")) {
			violations.push(
				...findModuleViolations(file.path, file.text, shipped).map((violation) => ({
					file: file.path,
					...violation,
				})),
			);
		} else if (file.path.endsWith(".html")) {
			violations.push(
				...findHtmlViolations(file.path, file.text, shipped).map((violation) => ({
					file: file.path,
					...violation,
				})),
			);
		} else if (file.path === "manifest.json") {
			violations.push(
				...findManifestViolations(file.text, shipped).map((violation) => ({
					file: file.path,
					...violation,
				})),
			);
		} else if (file.text.length > 0 && REMOTE_URL.test(file.text)) {
			violations.push({
				file: file.path,
				line: lineOf(file.text, file.text.search(REMOTE_URL)),
				class: "remote-url-literal",
				detail: "remote URL in a packaged file that is neither module, page nor manifest",
			});
		}
	}
	violations.push(...findRedirectTargetViolations());
	return violations;
}

/**
 * The remote URLs in one shipped module that the gate *exempted*, with the
 * exemption that covered each.
 *
 * Exposed so the test suite can assert the exemption is load-bearing rather
 * than vestigial: a silent drop of `PATTERN_MATRIX` from the data-only list, or
 * a ruleset rewrite that moves a URL out of a reviewed export, has to show up.
 */
export function exemptRemoteLiterals(file: BundleFile): {
	readonly value: string;
	readonly line: number;
	readonly exemptBy: string;
}[] {
	if (!file.path.endsWith(".js")) {
		return [];
	}
	return remoteLiteralsInModule(maskJs(file.text)).filter((literal) => literal.exemptBy !== "");
}

/** Human-readable report lines, for the CLI and for a failing assertion. */
export function describeViolations(violations: readonly BundleViolation[]): string {
	if (violations.length === 0) {
		return "clean: no remote code, no dynamic code, every reference resolves inside the package";
	}
	return violations
		.map(
			(violation) => `${violation.file}:${violation.line} [${violation.class}] ${violation.detail}`,
		)
		.join("\n");
}
