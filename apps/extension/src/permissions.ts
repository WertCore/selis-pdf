/**
 * SL-4.EXT.02 — PDF navigation interception.
 *
 * DNR rules that redirect `application/pdf` main-frame navigations to the
 * bundled viewer, preserving the original URL, the referrer policy, and the
 * auth context the browser would have sent.
 *
 * ## The constraint that shapes everything here
 *
 * EXT.01 ships `declarativeNetRequest` with an **empty** `host_permissions`,
 * and that stays. Without any host permission the ruleset only matches
 * requests the extension already has access to — which, for a main-frame
 * navigation to a third-party origin, is none of them. Two consequences
 * follow, and the design below is the honest response to both:
 *
 * 1. **We can match by URL shape, not by response type.** `responseHeaders`
 *    conditions need the same host access, so `Content-Type:
 *    application/pdf` and `Content-Disposition: inline` are *not* observable
 *    at match time. Rules therefore match the URL (a `.pdf` path, or a `.pdf`
 *    carried in the query), the signal available for free.
 * 2. **The patterns that need a response signal cannot work here.** They are
 *    enumerated in the matrix as `cannot-work` with the exact capability that
 *    would be required, so the limitation is named in code rather than
 *    discovered by a user. `file://` was the third documented non-case and
 *    stopped being one at SL-4.EXT.09: the `file` rule below is inert until the
 *    user grants file access, and the *detection* of that is the one thing this
 *    module deliberately cannot do — it has no browser. `src/local-files.ts`
 *    owns the grant, and `PERMISSIONS.md` carries the row.
 *
 * Nothing in this module widens permissions. If a pattern can only be served
 * by widening, it stays `cannot-work`.
 *
 * The module is pure — no `chrome.*` at import time — so the matrix is
 * testable under Vitest without a browser. `service-worker.js` is the only
 * place that touches the API.
 */

/** The bundled viewer page every intercepted navigation is sent to. */
export const VIEWER_PATH = "/viewer.html";

/** Query parameter carrying the original document URL into the viewer. */
export const SRC_PARAM = "src";

/**
 * Rule ids are stable and disjoint so a rule can be replaced or reasoned
 * about individually.
 */
export const RULE_ID = {
	/** A URL whose path ends in `.pdf`, ignoring query/fragment. */
	pdfPath: 1,
	/** A URL that only *carries* a `.pdf` in its query string. */
	pdfQuery: 2,
	/**
	 * A `file://` URL whose path ends in `.pdf` (SL-4.EXT.09).
	 *
	 * Its own id, and not a shared one, for two reasons a reviewer will ask
	 * about: it is installed on the same schedule as the other two but is inert
	 * until the user grants file access, and it is the one rule the service
	 * worker is willing to drop if Chrome rejects the whole ruleset (see
	 * `isFileRule` and `installInterception`).
	 */
	filePath: 3,
} as const;

/** A declarativeNetRequest redirect rule, in the shape the API accepts. */
export interface RedirectRule {
	readonly id: number;
	readonly priority: number;
	action: {
		readonly type: "redirect";
		readonly redirect: { readonly extensionPath: string };
	};
	condition: {
		readonly regexFilter: string;
		readonly resourceTypes: readonly string[];
	};
}

/**
 * The viewer URL for `documentUrl`.
 *
 * The original URL is passed as an encoded *parameter*, never interpolated
 * into the path: DNR's `extensionPath` cannot carry a raw `?`/`#`, and
 * hand-building that query is where URL-injection bugs live. A document URL
 * containing `&src=` cannot forge a second parameter, and a `#` cannot
 * truncate the real one.
 */
export function viewerUrlFor(documentUrl: string): string {
	return `${VIEWER_PATH}?${SRC_PARAM}=${encodeURIComponent(documentUrl)}`;
}

/**
 * Canonical MV3 permission set (SL-4.EXT.01).
 *
 * `manifest.json` is the shipped artefact; this module is the reviewable
 * contract the gate tests pin it against. Add a permission in exactly two
 * places — `manifest.json` + `PERMISSIONS.md` — or `manifest.test.ts` fails.
 * No third place exists on purpose: the allow-list below is the full set the
 * EXT.01 review approved.
 */

/** One row of the PDF-serving matrix: a real-world pattern and its verdict. */
export interface PatternVerdict {
	/** Human label for the serving pattern. */
	readonly name: string;
	/** The URL under test. */
	readonly url: string;
	/**
	 * The request's resource type. This is a real dimension, not decoration:
	 * the ruleset matches `main_frame` only, so the *same* `.pdf` URL is
	 * intercepted as a top-level navigation and untouched as a page's own
	 * `fetch()`. Rows that differ only by resource type say so.
	 */
	readonly resourceType?: "main_frame" | "sub_frame" | "xmlhttprequest" | "other";
	/**
	 * What DNR interception can do here, given the permissions EXT.01 approved.
	 */
	readonly outcome: /** Redirected to the viewer by URL shape. */
		| "intercepted"
		/** Not intercepted, and no permission we hold could change that. */
		| "cannot-work"
		/** Not intercepted by URL shape; see the reason. */
		| "not-matched";
	/** Why. */
	readonly reason: string;
}

/**
 * The rule set.
 *
 * `main_frame` is the only resource type: this is navigation interception,
 * and matching sub-resources would hijack `<embed>`/`<iframe>` PDFs the user
 * did not navigate to, plus every `fetch()` an app makes. A redirect
 * preserves the initiator and the request's credentials, so the referrer and
 * the browser's own auth still apply to the re-fetch.
 */
export function buildRedirectRules(): RedirectRule[] {
	return [
		{
			id: RULE_ID.pdfPath,
			priority: 1,
			action: { type: "redirect", redirect: { extensionPath: VIEWER_PATH } },
			condition: {
				// ^scheme://host[:port]/path ending in .pdf, optional ?query,
				// optional #fragment. (?i) on the extension only: `.PDF` is
				// common in the wild and case must not decide behaviour.
				regexFilter: "^https?://[^?#\\s]+(?i:\\.pdf)(?:[?#]\\S*)?$",
				resourceTypes: ["main_frame"],
			},
		},
		{
			id: RULE_ID.pdfQuery,
			priority: 1,
			action: { type: "redirect", redirect: { extensionPath: VIEWER_PATH } },
			condition: {
				// The path does not end in .pdf but the query carries one — the
				// "?file=report.pdf" family. The lookahead keeps the two rules
				// disjoint so no URL is claimed twice.
				regexFilter:
					"^https?://[^?#]*[^?#]*(?![^?#]*(?i:\\.pdf)(?:[?#]|$))[?#][^#]*(?i:\\.pdf)(?:[&#][^#]*)?$",
				resourceTypes: ["main_frame"],
			},
		},
		{
			id: RULE_ID.filePath,
			priority: 1,
			action: { type: "redirect", redirect: { extensionPath: VIEWER_PATH } },
			condition: {
				// SL-4.EXT.09. The same shape as the http(s) path rule, with the
				// scheme fixed to `file:` and no authority to skip: a local URL
				// is `file:///C:/…`, so the two characters after the scheme are
				// slashes and the third begins the path.
				//
				// **This rule only fires once the user has granted file access.**
				// DNR evaluates a rule only against a request the extension may
				// already touch, so before the grant it matches nothing at all -
				// which is the whole reason the local-file flow needs a detection
				// of its own rather than an interception: nothing intercepts, so
				// nothing can report having failed. `src/local-files.ts` owns the
				// grant; `fileAccessState()` is what the viewer asks.
				//
				// The extension is on the *other* side of the redirect too, so
				// this is not a case of the extension being unable to see a local
				// file it is entitled to: with the toggle on, it opens them.
				regexFilter: "^file://[^?#\\s]*(?i:\\.pdf)(?:[?#]\\S*)?$",
				resourceTypes: ["main_frame"],
			},
		},
	];
}

/**
 * Is this the one rule that only fires with file access granted?
 *
 * Exported so the service worker's degraded install can drop exactly this rule
 * and keep the two http(s) ones. That fallback exists because a rejected
 * `updateDynamicRules` takes the *whole* call with it: if any Chrome build
 * refuses the `file` rule while the grant is withheld, the rules that intercept
 * every web PDF would go down with it, and a viewer that stopped working for
 * web PDFs because of an optional local-file feature is a strictly worse
 * product than one that never learns to open local files.
 */
export function isFileRule(rule: RedirectRule): boolean {
	return rule.id === RULE_ID.filePath;
}

/**
 * RE2-backed `RegExp` with a guard: a malformed rule must fail the assertion
 * rather than throw inside the service worker.
 */
function safeMatch(pattern: string, value: string): boolean {
	try {
		return new RegExp(pattern).test(value);
	} catch {
		return false;
	}
}

/**
 * The ~30 real-world PDF-serving patterns (the task's Risk note), each with
 * its verdict.
 *
 * This is the matrix the tests assert against: a rule change that silently
 * regresses a pattern fails here, by name, instead of in a support ticket.
 * The `cannot-work` rows are the DoD's "document the cases that cannot work"
 * half, and each names the capability that would be required.
 */
export const PATTERN_MATRIX: readonly PatternVerdict[] = [
	// ── Direct .pdf URLs: the common case ────────────────────────────────
	{
		name: "direct .pdf, no query",
		url: "https://example.com/docs/report.pdf",
		outcome: "intercepted",
		reason: "the path ends in .pdf, so the path rule matches",
	},
	{
		name: "direct .pdf with query string",
		url: "https://example.com/docs/report.pdf?download=1&v=2",
		outcome: "intercepted",
		reason: "path ends in .pdf; the query rides along in the src parameter",
	},
	{
		name: "direct .pdf with fragment",
		url: "https://example.com/docs/report.pdf#page=14",
		outcome: "intercepted",
		reason: "path ends in .pdf; the fragment rides along",
	},
	{
		name: "uppercase .PDF",
		url: "https://example.com/docs/REPORT.PDF",
		outcome: "intercepted",
		reason: "the (?i) group matches the extension case-insensitively",
	},
	{
		name: "mixed-case .Pdf",
		url: "https://example.com/docs/Report.Pdf",
		outcome: "intercepted",
		reason: "case-insensitive, as above",
	},
	{
		name: ".pdf on a non-standard port",
		url: "https://example.com:8443/docs/report.pdf",
		outcome: "intercepted",
		reason: "the port is part of [^?#\\s]+; the path still ends in .pdf",
	},
	{
		name: ".pdf over http (not https)",
		url: "http://example.com/docs/report.pdf",
		outcome: "intercepted",
		reason: "the scheme alternation accepts http",
	},
	{
		name: ".pdf on a CDN subdomain",
		url: "https://files.cdn.example.com/a/b/c/manual.pdf",
		outcome: "intercepted",
		reason: "a nested path still ends in .pdf",
	},
	{
		name: ".pdf with a percent-encoded space in the path",
		url: "https://example.com/docs/quarterly%20report.pdf",
		outcome: "intercepted",
		reason: "the encoded space is part of [^?#\\s]+; .pdf still terminates it",
	},
	{
		name: ".pdf on an IDN (punycode) host",
		url: "https://xn--bcher-kva.example/docs/report.pdf",
		outcome: "intercepted",
		reason: "punycode is ordinary URL text to the regex",
	},
	{
		name: ".pdf at the host root",
		url: "https://example.com/report.pdf",
		outcome: "intercepted",
		reason: "the shortest form of the same pattern",
	},
	{
		name: ".pdf with a unicode path segment",
		url: "https://example.com/rapport/épreuve.pdf",
		outcome: "intercepted",
		reason: "non-ASCII path text does not affect the .pdf terminator",
	},
	// ── Query-carried PDFs ───────────────────────────────────────────────
	{
		name: "?file=report.pdf on a viewer page",
		url: "https://example.com/viewer?file=report.pdf",
		outcome: "intercepted",
		reason: "the query-carried rule matches",
	},
	{
		name: "?doc= with several params, one .pdf",
		url: "https://example.com/print?a=1&doc=x.pdf&b=2",
		outcome: "intercepted",
		reason: "a .pdf anywhere in the query matches",
	},
	{
		name: "?url= with a percent-encoded .pdf inside it",
		url: "https://example.com/open?url=https%3A%2F%2Fx.test%2Fa.pdf",
		outcome: "intercepted",
		reason:
			"the query-carried rule matches: the .pdf is percent-encoded inside a value, but the raw query text still ends in it, and a viewer that fetches a PDF is a viewer. The inner URL is carried intact in src, so it still opens",
	},
	{
		name: "?format=pdf without a dot",
		url: "https://example.com/render?format=pdf&width=200",
		outcome: "not-matched",
		reason:
			"deliberately narrow: a bare ?format=pdf would hijack ordinary HTML pages that merely print to PDF",
	},
	// ── Content-Type only: the class DNR cannot see ───────────────────────
	{
		name: "Content-Type: application/pdf, extensionless URL",
		url: "https://api.example.com/v2/documents/9f3c",
		outcome: "cannot-work",
		reason:
			"the response type is the only signal, and responseHeaders conditions need host access we do not hold; requires host_permissions for the origin",
	},
	{
		name: "Content-Type: application/pdf, numeric id path",
		url: "https://example.com/download/12345",
		outcome: "cannot-work",
		reason:
			"as the row above: the response type is the only signal, and matching it needs host permissions for the origin",
	},
	{
		name: "Content-Type: application/pdf on a REST endpoint",
		url: "https://example.com/api/v1/invoices/2026/q1",
		outcome: "cannot-work",
		reason:
			"as above — a responseHeaders condition needs host permissions for the origin, which EXT.01 declines",
	},
	{
		name: "application/octet-stream served at a .pdf path",
		url: "https://example.com/blob/report.pdf",
		outcome: "intercepted",
		reason: "the .pdf path is enough; the served type is irrelevant to a redirect",
	},
	{
		name: "text/html served at a .pdf path (soft 404)",
		url: "https://example.com/error/report.pdf",
		outcome: "intercepted",
		reason:
			"URL-shape matching cannot distinguish a soft-404 page from a PDF. The viewer reports a parse failure instead of the browser showing the page — a known, accepted trade",
	},
	// ── Content-Disposition ──────────────────────────────────────────────
	{
		name: "Content-Disposition: inline, .pdf path",
		url: "https://example.com/files/spec.pdf",
		outcome: "intercepted",
		reason: ".pdf path; inline vs attachment is invisible to us and irrelevant here",
	},
	{
		name: "Content-Disposition: attachment, .pdf path",
		url: "https://example.com/files/invoice.pdf",
		outcome: "intercepted",
		reason:
			"the browser would have downloaded it; we now show it. Deliberate — the extension views PDFs and the viewer never uploads or writes",
	},
	{
		name: "attachment; filename=report.pdf, extensionless URL",
		url: "https://example.com/att/8471",
		outcome: "cannot-work",
		reason:
			"the filename lives in response headers, unreachable without host permissions for the origin",
	},
	// ── POST-produced PDFs ───────────────────────────────────────────────
	{
		name: "POST form submit returns a PDF (extensionless)",
		url: "https://example.com/report/generate",
		outcome: "cannot-work",
		reason:
			"the URL is extensionless and a POST response is not re-navigated by the browser, so neither the URL shape nor the request type gives a signal we can match",
	},
	{
		name: "POST to a .pdf-looking URL",
		url: "https://example.com/generate.pdf",
		outcome: "intercepted",
		reason:
			"the .pdf path matches on URL alone; the redirect preserves the request, so the viewer re-issues it — a UI concern, not a DNR one",
	},
	// ── Redirects ────────────────────────────────────────────────────────
	{
		name: "302 redirect to a .pdf",
		url: "https://example.com/go?id=42",
		outcome: "not-matched",
		reason:
			"the first navigation is not a .pdf URL, and DNR does not re-evaluate rules per redirect hop, so the viewer never sees it",
	},
	{
		name: "URL shortener resolving to a .pdf",
		url: "https://t.co/abc123",
		outcome: "cannot-work",
		reason:
			"indistinguishable from any other short URL before the response; the path carries no .pdf and no host permission would expose the response",
	},
	// ── Non-network surfaces ─────────────────────────────────────────────
	{
		name: "blob: URL (print-to-PDF round trip)",
		url: "blob:https://example.com/550e8400-e29b-41d4-a716-446655440000",
		outcome: "cannot-work",
		reason: "blob: is not a network request, and the scheme is outside ^https?://",
	},
	{
		name: "data: URL containing a PDF",
		url: "data:application/pdf;base64,JVBERi0xLjQK",
		outcome: "cannot-work",
		reason: "data: is not a network request and the scheme is outside the rule",
	},
	{
		name: "file:// local PDF",
		url: "file:///C:/Users/x/Documents/report.pdf",
		outcome: "intercepted",
		reason:
			"SL-4.EXT.09: a dedicated file:// rule redirects it, but only once the user has granted 'Allow access to file URLs'. Until then the rule is inert and Chrome's own viewer shows the file - which is why the extension detects the grant with chrome.extension.isAllowedFileSchemeAccess() rather than waiting for an interception that will not happen",
	},
	{
		name: "file:// local HTML page",
		url: "file:///C:/Users/x/Documents/notes.html",
		outcome: "not-matched",
		reason:
			"the file:// rule is .pdf-shaped on URL alone, exactly like the http(s) one, so a page on disk is left to the browser even with file access granted",
	},
	{
		name: "file:// PDF with a query string",
		url: "file:///C:/Users/x/Documents/report.pdf?page=14",
		outcome: "intercepted",
		reason:
			"the file:// rule accepts the same optional [?#] tail the http(s) path rule does, and there is no file:// query rule because a local path has no query to carry a .pdf",
	},
	{
		name: "WebSocket upgrade to a .pdf-named path",
		url: "wss://example.com/stream/report.pdf",
		outcome: "cannot-work",
		reason: "not a main-frame navigation and the scheme is outside the rule",
	},
	// ── Deliberate non-matches ───────────────────────────────────────────
	{
		name: "<embed> of a .pdf inside another page",
		url: "https://example.com/embed/report.pdf",
		outcome: "not-matched",
		resourceType: "sub_frame",
		reason:
			"an embed is a sub_frame and the rules are main_frame only, so we do not hijack a document the user did not navigate to",
	},
	{
		name: "page fetch()es a .pdf itself",
		url: "https://example.com/docs/report.pdf",
		outcome: "not-matched",
		resourceType: "xmlhttprequest",
		reason:
			"the same URL that IS intercepted as a navigation: an xmlhttprequest is not main_frame, so an app's own download keeps working and is never redirected",
	},
	{
		name: "malformed URL containing a space",
		url: "https://example.com/a b.pdf",
		outcome: "not-matched",
		reason:
			"[^?#\\s]+ excludes whitespace, so a malformed URL cannot be redirected into a parameter-smuggling shape",
	},
	// ── Auth and signed URLs: the redirect must preserve them ─────────────
	{
		name: "basic-auth protected PDF",
		url: "https://user:pass@example.com/docs/report.pdf",
		outcome: "intercepted",
		reason:
			"a redirect preserves the request's credentials and the viewer re-fetches with credentials: include, so the browser's own auth still applies",
	},
	{
		name: "cookie-gated PDF behind SSO",
		url: "https://example.com/sso/report.pdf",
		outcome: "intercepted",
		reason:
			"cookies ride along with the redirected request; a missing session surfaces as an auth state, not a blank tab",
	},
	{
		name: "signed (AWS presigned) URL",
		url: "https://bucket.s3.amazonaws.com/a.pdf?X-Amz-Signature=deadbeef",
		outcome: "intercepted",
		reason:
			"the full original URL including the signature is carried in the src parameter, so the re-fetch stays valid; a truncated URL would 403",
	},
	{
		name: "very long query carrying a .pdf",
		url: "https://example.com/view?x=aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa&f=b.pdf",
		outcome: "intercepted",
		reason:
			"the regex has no length limit (Chrome caps a ruleset regex at 2 KB, far above this); pathological inputs are documented rather than special-cased",
	},
];

/** Patterns that no permission we hold can serve — the documented limits. */
export const UNSUPPORTED_PATTERNS: readonly PatternVerdict[] = PATTERN_MATRIX.filter(
	(v) => v.outcome === "cannot-work",
);

/** True when `url` is matched by the shipped ruleset. */
export function isIntercepted(
	url: string,
	rules: readonly RedirectRule[] = buildRedirectRules(),
): boolean {
	return rules.some((rule) => safeMatch(rule.condition.regexFilter, url));
}

/**
 * True when the *request* is intercepted: the URL matches a rule **and** the
 * rule's `resourceTypes` include the request's type.
 *
 * `isIntercepted(url)` alone answers "does the URL match?", which is not the
 * same question — a `.pdf` URL fetched by the page matches the regex but is
 * never redirected, because the rules are `main_frame` only. The matrix rows
 * that turn on this distinction carry a `resourceType`.
 */
export function isRequestIntercepted(
	url: string,
	resourceType: "main_frame" | "sub_frame" | "xmlhttprequest" | "other",
	rules: readonly RedirectRule[] = buildRedirectRules(),
): boolean {
	return rules.some(
		(rule) =>
			rule.condition.resourceTypes.includes(resourceType) &&
			safeMatch(rule.condition.regexFilter, url),
	);
}

/**
 * Permissions EXT.01 approves. Anything else fails the manifest gate.
 *
 * `storage` arrived with SL-4.EXT.05, which is the task that decided the CJK
 * payload is an optional post-install download into extension storage rather
 * than a bundled asset — a decision that is unimplementable without it. It is
 * the plain `storage` permission and deliberately **not** `unlimitedStorage`:
 * `storage.local`'s default 10 MB quota is the ceiling the payload store is
 * budgeted against (`ext/cjk-payload.ts`), so the wider permission has not
 * been earned. `PERMISSIONS.md` carried the forward reference from EXT.01.
 */
export const ALLOWED_PERMISSIONS: readonly string[] = [
	"declarativeNetRequest",
	"offscreen",
	"storage",
];

/**
 * Host patterns EXT.01 approves **at install**. Still empty, and still empty
 * after SL-4.EXT.09.
 *
 * This is the load-bearing half of the local-file decision. `file:///` went into
 * `optional_host_permissions` instead, which is what a match pattern in
 * `host_permissions` would have cost: Chrome documents that "adding or changing
 * match patterns in the `host_permissions` field will trigger a warning", and
 * `file:///` is the pattern a reviewer reads as "reads your disk". Nothing is
 * granted until the user acts, so a warning that says so before the user has
 * asked for anything is the warning EXT.01 was written to avoid.
 */
export const ALLOWED_HOST_PERMISSIONS: readonly string[] = [];

/**
 * Host patterns the extension may declare as **optional** (SL-4.EXT.09).
 *
 * One pattern, and the three-slash form is load-bearing. Chrome's match-pattern
 * reference documents the local-file special case as `file:///` — "allows your
 * extension to run on local files… this pattern requires the user to manually
 * grant access" — and calls out that it "requires three slashes, not two". The
 * two-slash spellings are in {@link DENIED_HOST_PATTERNS} and are not a narrower
 * version of this one; they are a different and invalid pattern.
 *
 * Declaring it is not the same as holding it. `host_permissions` stays `[]`,
 * `ALLOWED_PERMISSIONS` is unchanged, and until the user turns the toggle on
 * this grants nothing — and the extension calls no permission API at all, so
 * there is no prompt to dismiss and nothing to request. `PERMISSIONS.md` has the
 * full argument and the store wording, and `src/local-files.ts` has the flow.
 */
export const ALLOWED_OPTIONAL_HOST_PERMISSIONS: readonly string[] = ["file:///"];

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

/**
 * Broad host patterns that always fail review without a wired rule.
 *
 * The two `file:` entries are the **two-slash** spellings, and they are here
 * because they are a common mistake rather than a scope claim: Chrome's
 * match-pattern reference is explicit that the local-file pattern "requires
 * three slashes, not two", so the two spellings below are not narrow versions
 * of the pattern this extension declares — they are a different, invalid
 * pattern. (They are spelled here without their trailing wildcard, because
 * writing the full form inside a block comment would close the comment.) The
 * pattern the extension does declare is in
 * {@link ALLOWED_OPTIONAL_HOST_PERMISSIONS}, it is optional, and
 * `manifest.test.ts` asserts the two spellings never appear in the manifest
 * beside it.
 */
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
	readonly optional_host_permissions?: unknown;
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
	const optionalHostPermissions = Array.isArray(manifest.optional_host_permissions)
		? manifest.optional_host_permissions
		: null;
	if (optionalHostPermissions === null) {
		if (manifest.optional_host_permissions !== undefined) {
			violations.push({
				field: "optional_host_permissions",
				reason: "must be an array when present",
			});
		}
	} else {
		// The same two checks as `host_permissions`, plus the requirement that
		// the list is not merely *valid* but *complete*: an approved pattern that
		// quietly stopped shipping would leave the viewer's local-file flow
		// explaining a permission the extension no longer declares, and
		// `local-files.test.ts` would still be green because the runtime object
		// is a constant nobody re-derived from the manifest.
		for (const pattern of optionalHostPermissions) {
			if (typeof pattern !== "string") {
				violations.push({
					field: "optional_host_permissions",
					reason: "every entry must be a string",
				});
				continue;
			}
			if (DENIED_HOST_PATTERNS.includes(pattern)) {
				violations.push({
					field: "optional_host_permissions",
					reason: `'${pattern}' is a broad host pattern, and the two-slash file spellings are not patterns at all (Chrome: "requires three slashes, not two")`,
				});
			}
			if (!ALLOWED_OPTIONAL_HOST_PERMISSIONS.includes(pattern)) {
				violations.push({
					field: "optional_host_permissions",
					reason: `'${pattern}' has no PERMISSIONS.md justification row — add one in review (EXT.09) or drop it`,
				});
			}
		}
		for (const required of ALLOWED_OPTIONAL_HOST_PERMISSIONS) {
			if (!optionalHostPermissions.includes(required)) {
				violations.push({
					field: "optional_host_permissions",
					reason: `'${required}' is the declared local-file flow (SL-4.EXT.09); the viewer asks for it by name, so dropping it ships a page that asks for a grant it cannot offer`,
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
