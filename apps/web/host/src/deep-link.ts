/**
 * Local-only deep links into the web app for edit actions (SL-4.EXT.08).
 *
 * "Edit this in the web app" hands a *local handle* to the web shell — an OPFS
 * path or a same-origin transfer id — never document bytes and never an upload
 * (24-BINDINGS-SPEC §5, ADR-P0016). A link is therefore only an instruction:
 *
 *   https://pdf.selis.com/edit-pdf?action=edit&doc=<opaque-handle-id>
 *
 * The document id is an opaque token the host resolves against storage it
 * already owns. Nothing in this module opens storage, fetches a URL, or reads
 * a file; it parses and *refuses* malformed links so a hostile URL cannot turn
 * a local handoff into an exfiltration vector (a `data:` body, a `file:` path,
 * a traversal sequence, or an embedded PDF). The same strictness applies to
 * the canonical builder, so a link this module writes always parses back.
 *
 * This module is DOM-free and pure so Vitest runs it in Node; the URL parser
 * is the only platform primitive used, and it is available in every port
 * (Firefox and Safari included — see capability.ts for the rest of the host).
 */

/** The only action a deep link can request here. */
export const EDIT_ACTION = "edit" as const;

/** Handle kinds a web shell can resolve locally (never a network fetch). */
export type LocalHandleKind = "opfs" | "fsa";

/** A validated edit deep link. */
export interface EditDeepLink {
	readonly action: typeof EDIT_ACTION;
	readonly documentId: string;
	readonly handle: LocalHandleKind;
	readonly href: string;
}

/** A refused link, with a reason that never includes document bytes. */
export interface DeepLinkRefusal {
	readonly ok: false;
	readonly reason: string;
}

/** Parse result: a typed edit action or an explicit refusal. */
export type EditDeepLinkResult =
	| ({ readonly ok: true } & EditDeepLink)
	| DeepLinkRefusal;

/**
 * Opaque handle ids: URL-safe, no traversal, no scheme, no bytes. The first
 * character must be alphanumeric so a bare `.` or `-` cannot be a handle.
 */
const DOC_ID_RE = /^[A-Za-z0-9][A-Za-z0-9_-]{0,127}$/;

function refuse(reason: string): DeepLinkRefusal {
	return { ok: false, reason };
}

/**
 * Parse a full URL (or a root-relative reference against `appHref`) into an
 * edit action plus a validated local document id.
 *
 * Refusals are exhaustive and fail closed:
 * - an unparseable URL, or one with a non-http(s) scheme / userinfo;
 * - a missing or unexpected `action` (only `edit` is recognised);
 * - a missing, malformed, or over-long `doc` id;
 * - an unknown `handle` kind;
 * - any parameter that looks like it carries document bytes (`data:`,
 *   `%PDF-`, a `blob:`, `file:` or `javascript:` scheme).
 *
 * A missing link is a non-error no-op with a reason, so a caller can ignore it
 * without treating "no deep link" as an attack.
 */
export function parseEditDeepLink(href: string, appHref?: string): EditDeepLinkResult {
	let url: URL;
	try {
		url = appHref === undefined ? new URL(href) : new URL(href, appHref);
	} catch {
		return refuse("not a URL");
	}
	if (url.protocol !== "http:" && url.protocol !== "https:") {
		return refuse(`rejected deep-link scheme "${url.protocol}"`);
	}
	if (url.username !== "" || url.password !== "") {
		return refuse("userinfo is not allowed in a deep link");
	}

	// The raw link must never look like it carries bytes or a handle masquerading
	// as a URL. Check both the encoded whole string and the decoded parameter
	// values, so `%25PDF-` (an encoded `%PDF-`) cannot slip through.
	const suspicious = (text: string): boolean => {
		const lower = text.toLowerCase();
		return (
			lower.includes("data:") ||
			lower.includes("%pdf-") ||
			lower.includes("blob:") ||
			lower.startsWith("javascript:") ||
			lower.includes("file:")
		);
	};
	if (suspicious(url.href) || [...url.searchParams.values()].some(suspicious)) {
		return refuse("deep link carries bytes or a non-local handle");
	}

	const action = url.searchParams.get("action");
	if (action === null || action.trim() === "") {
		return refuse("no ?action= parameter");
	}
	if (action !== EDIT_ACTION) {
		return refuse(`unknown action "${action.slice(0, 32)}"`);
	}

	const doc = url.searchParams.get("doc");
	if (doc === null || doc.trim() === "") {
		return refuse("no ?doc= parameter");
	}
	if (!DOC_ID_RE.test(doc)) {
		return refuse("malformed document id");
	}

	const handleRaw = url.searchParams.get("handle");
	let handle: LocalHandleKind = "opfs";
	if (handleRaw !== null && handleRaw.trim() !== "") {
		if (handleRaw !== "opfs" && handleRaw !== "fsa") {
			return refuse(`unknown handle kind "${handleRaw.slice(0, 32)}"`);
		}
		handle = handleRaw;
	}

	return {
		ok: true,
		action: EDIT_ACTION,
		documentId: doc,
		handle,
		href: url.href,
	};
}

/**
 * Build a canonical edit deep link. The id is validated by the same rule the
 * parser enforces, so the two cannot drift into disagreement.
 */
export function buildEditDeepLink(
	baseHref: string,
	documentId: string,
	handle: LocalHandleKind = "opfs",
): string {
	if (!DOC_ID_RE.test(documentId)) {
		throw new Error("malformed document id");
	}
	const url = new URL(baseHref);
	url.search = "";
	url.hash = "";
	url.searchParams.set("action", EDIT_ACTION);
	url.searchParams.set("doc", documentId);
	url.searchParams.set("handle", handle);
	return url.href;
}
