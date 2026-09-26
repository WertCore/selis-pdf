/**
 * Canonical MV3 permission set (SL-4.EXT.01).
 *
 * `manifest.json` is the shipped artefact; this module is the reviewable
 * contract the gate tests pin it against. Add a permission in exactly two
 * places — `manifest.json` + `PERMISSIONS.md` — or `manifest.test.ts` fails.
 * No third place exists on purpose: the allow-list below is the full set the
 * EXT.01 review approved.
 */

/** Permissions EXT.01 approves. Anything else fails the manifest gate. */
export const ALLOWED_PERMISSIONS: readonly string[] = ["declarativeNetRequest", "offscreen"];

/**
 * Host patterns EXT.01 approves. Empty on purpose: DNR interception targets
 * (SL-4.EXT.02) add narrow entries here with their own PERMISSIONS.md rows —
 * never a pre-emptive `<all_urls>`.
 */
export const ALLOWED_HOST_PERMISSIONS: readonly string[] = [];

/** Permissions that must never appear (broad interception / injection). */
export const DENIED_PERMISSIONS: readonly string[] = [
	"webRequest",
	"webRequestBlocking",
	"tabs",
	"activeTab",
	"scripting",
	"cookies",
	"unlimitedStorage",
	"debugger",
	"proxy",
	"privacy",
];

/** Broad host patterns that always fail review without a wired rule. */
export const DENIED_HOST_PATTERNS: readonly string[] = [
	"<all_urls>",
	"*://*/*",
	"http://*/*",
	"https://*/*",
	"file://*/*",
	"file:///*",
];

/** Shape of the MV3 manifest fields this gate checks. */
export interface ExtensionManifestShape {
	readonly manifest_version?: unknown;
	readonly permissions?: unknown;
	readonly host_permissions?: unknown;
	readonly background?: { readonly service_worker?: unknown } | null;
	readonly content_security_policy?: {
		readonly extension_pages?: unknown;
	} | null;
}

/** One gate failure: field + human-readable reason. */
export interface ManifestViolation {
	readonly field: string;
	readonly reason: string;
}

/**
 * Validate a parsed `manifest.json` against the EXT.01 minimal-permission
 * contract. Returns every violation (empty = pass). Pure: no fs, no globals.
 */
export function validateManifest(manifest: ExtensionManifestShape): ManifestViolation[] {
	const violations: ManifestViolation[] = [];
	if (manifest.manifest_version !== 3) {
		violations.push({
			field: "manifest_version",
			reason: "must be 3 (MV3 only; MV2 is rejected by the store)",
		});
	}
	const permissions = Array.isArray(manifest.permissions) ? manifest.permissions : null;
	if (permissions === null) {
		violations.push({ field: "permissions", reason: "must be an array" });
	} else {
		for (const permission of permissions) {
			if (typeof permission !== "string") {
				violations.push({ field: "permissions", reason: "every entry must be a string" });
				continue;
			}
			if (!ALLOWED_PERMISSIONS.includes(permission)) {
				violations.push({
					field: "permissions",
					reason: `'${permission}' is not in the EXT.01 approved set (${ALLOWED_PERMISSIONS.join(", ")}) — add a PERMISSIONS.md row and extend ALLOWED_PERMISSIONS in review, or drop it`,
				});
			}
			if (DENIED_PERMISSIONS.includes(permission)) {
				violations.push({
					field: "permissions",
					reason: `'${permission}' is a broad capability EXT.01 explicitly refuses (see PERMISSIONS.md "Deliberately NOT requested")`,
				});
			}
		}
		for (const required of ALLOWED_PERMISSIONS) {
			if (!permissions.includes(required)) {
				violations.push({
					field: "permissions",
					reason: `'${required}' is required by the EXT.01 architecture (see PERMISSIONS.md)`,
				});
			}
		}
	}
	const hostPermissions = Array.isArray(manifest.host_permissions)
		? manifest.host_permissions
		: null;
	if (hostPermissions === null) {
		violations.push({ field: "host_permissions", reason: "must be an array (empty in EXT.01)" });
	} else {
		for (const pattern of hostPermissions) {
			if (typeof pattern !== "string") {
				violations.push({ field: "host_permissions", reason: "every entry must be a string" });
				continue;
			}
			if (DENIED_HOST_PATTERNS.includes(pattern)) {
				violations.push({
					field: "host_permissions",
					reason: `'${pattern}' is a broad host pattern: EXT.01 ships none — narrow DNR targets land in EXT.02 with per-pattern justification`,
				});
			}
			if (!ALLOWED_HOST_PERMISSIONS.includes(pattern)) {
				violations.push({
					field: "host_permissions",
					reason: `'${pattern}' has no PERMISSIONS.md justification row — add one in review (EXT.02) or drop it`,
				});
			}
		}
	}
	const worker = manifest.background?.service_worker;
	if (typeof worker !== "string" || worker.length === 0) {
		violations.push({
			field: "background.service_worker",
			reason: "MV3 requires a service worker entry (event-driven, no DOM)",
		});
	}
	const csp = manifest.content_security_policy?.extension_pages;
	if (typeof csp !== "string") {
		violations.push({
			field: "content_security_policy.extension_pages",
			reason: "must pin the extension-pages CSP (ADR-P0028: no remote code)",
		});
	} else {
		if (!csp.includes("script-src 'self'")) {
			violations.push({
				field: "content_security_policy.extension_pages",
				reason: "must contain \"script-src 'self'\" (bundled code only)",
			});
		}
		if (
			/https?:\/\//.test(csp) ||
			csp.includes("'unsafe-eval'") ||
			csp.includes("'unsafe-inline'")
		) {
			violations.push({
				field: "content_security_policy.extension_pages",
				reason: "must not allow remote hosts, unsafe-eval, or unsafe-inline (ADR-P0028)",
			});
		}
	}
	return violations;
}
