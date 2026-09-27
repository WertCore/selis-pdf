/**
 * SL-4.EXT.02 - the redirect target's side of the interception.
 *
 * The DNR rule rewrites the browser's address bar to `viewer.html?src=<url>`,
 * so this module's job is to recover the document URL exactly as the browser
 * sent it, and to fetch it with the browser's own credentials so the auth the
 * user already has (basic-auth, SSO cookies, a presigned signature) survives
 * the redirect. The engine and UI land with SL-4.EXT.06; what EXT.02 owns is
 * the URL arriving intact and the document never being uploaded.
 */

import { SRC_PARAM } from "./permissions.js";

/** Why a viewer session has no document, for the SL-4.UI.12 error state. */
export class ViewerSourceError extends Error {
	constructor(
		readonly reason: "missing-src" | "invalid-src" | "not-fetchable",
		message: string,
	) {
		super(message);
		this.name = "ViewerSourceError";
	}
}

/**
 * The document URL this viewer session should open.
 *
 * Round-trips through `URLSearchParams`, so a document URL carrying `?`, `#`
 * or its own `&src=` comes back byte-identical to what the rule encoded. Only
 * http(s) is accepted: `blob:`, `data:` and `file:` are the documented
 * cannot-work cases, and fetching them here would either fail or silently
 * read local state the extension was never granted.
 */
export function documentUrlFromViewerLocation(search: string): string {
	const raw = new URLSearchParams(search).get(SRC_PARAM);
	if (raw === null) {
		throw new ViewerSourceError(
			"missing-src",
			"the viewer was opened without a document URL - use the toolbar button",
		);
	}
	let parsed: URL;
	try {
		parsed = new URL(raw);
	} catch {
		throw new ViewerSourceError("invalid-src", "the document URL is not a valid URL");
	}
	if (parsed.protocol !== "http:" && parsed.protocol !== "https:") {
		throw new ViewerSourceError("not-fetchable", `cannot open a ${parsed.protocol} document`);
	}
	return parsed.toString();
}

/**
 * The fetch the viewer uses for an intercepted document.
 *
 * `credentials: "include"` is the load-bearing part: the redirect preserved
 * the browser's cookies and the original request's auth, and dropping the
 * credentials here would turn every SSO-gated and basic-auth PDF into a login
 * prompt. `redirect: "follow"` lets a 302 chain resolve, which is the one
 * redirect case the DNR rule cannot see (see PATTERN_MATRIX).
 *
 * The response is never uploaded or persisted here: it is returned to the
 * caller and handed to the engine in-process.
 */
export function fetchInterceptedDocument(url: string): Promise<Response> {
	return fetch(url, {
		credentials: "include",
		redirect: "follow",
		referrerPolicy: "strict-origin-when-cross-origin",
	});
}
