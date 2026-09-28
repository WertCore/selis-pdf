/**
 * SL-4.EXT.02 - the redirect target's side of the interception.
 *
 * The DNR rule rewrites the browser's address bar to `viewer.html?src=<url>`,
 * so this module's job is to recover the document URL exactly as the browser
 * sent it, and to fetch it with the browser's own credentials so the auth the
 * user already has (basic-auth, SSO cookies, a presigned signature) survives
 * the redirect. SL-4.EXT.06 adds the composition that opens that document
 * behind the extension adapter; what EXT.02 owns is the URL arriving intact
 * and the document never being uploaded.
 */

import { createBrowserViewerSession, describeFailure, openDocument } from "./ext/viewer-session.js";
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

/**
 * Bring the viewer page up: session, document, and a line the user can read.
 *
 * Every failure path here ends in the same place - a message in the page, with
 * the registry code kept - because a viewer that silently shows nothing is the
 * one failure mode the seam cannot excuse. `describeFailure` is what turns a
 * rejection into that message; the alternative is a bare `catch {}`, which is
 * what EXT.01 shipped and which is precisely why nothing was visible.
 */
export async function bootViewer(container: HTMLElement): Promise<void> {
	report(container, "Starting the Selis engine...");

	let documentUrl: string;
	try {
		documentUrl = documentUrlFromViewerLocation(globalThis.location.search);
	} catch (error) {
		report(container, error instanceof Error ? error.message : "no document to open");
		return;
	}

	try {
		// The one network request this extension makes, and it is a GET of the
		// document the user asked for, with the credentials the browser would
		// have sent. Nothing is uploaded (EXT.02, and the web app's no-upload
		// gate has the same rule for the same reason).
		const response = await fetchInterceptedDocument(documentUrl);
		if (!response.ok) {
			report(container, `The document server answered ${response.status}.`);
			return;
		}
		const bytes = await response.arrayBuffer();

		const { adapter } = await createBrowserViewerSession();
		const name = fileNameFromUrl(documentUrl);
		const { doc } = await openDocument(adapter, { kind: "bytes", bytes, name });
		report(container, `Opened ${name} - ${doc.pageCount} page${doc.pageCount === 1 ? "" : "s"}.`);
	} catch (error) {
		const failure = describeFailure(error);
		report(container, failure.message);
	}
}

/**
 * The document's file name, for the title and the page count line.
 *
 * Derived from the URL's path rather than fetched from a `Content-Disposition`
 * header: the name is only ever shown to the user, and a header the server
 * controls is a name the server chose. It is never used as a path.
 */
export function fileNameFromUrl(documentUrl: string): string {
	try {
		const last = new URL(documentUrl).pathname.split("/").filter(Boolean).at(-1);
		return last === undefined || last.length === 0 ? "document.pdf" : decodeURIComponent(last);
	} catch {
		return "document.pdf";
	}
}

/** Write one line of status into the page. */
function report(container: HTMLElement, message: string): void {
	container.textContent = message;
}

/** Run the viewer when this module is loaded as the page's entry point. */
if (typeof document !== "undefined") {
	const mount = document.getElementById("selis-viewer");
	if (mount !== null) {
		void bootViewer(mount);
	}
}
