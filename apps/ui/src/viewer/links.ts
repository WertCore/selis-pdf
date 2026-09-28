/**
 * Link activation and ADR-P0020 (SL-4.UI.06).
 *
 * The plan's DoD for this task is one sentence — *"Link annotations are
 * activated here but obey ADR-P0020: external URIs prompt with the full
 * destination shown, and `/Launch` is refused"* — and this file is where both
 * halves of it are decided. Everything is pure: a link annotation goes in, a
 * {@link LinkDecision} comes out, and the controller in `navigation.ts` is the
 * only thing that acts on one.
 *
 * ## The policy, in one table
 *
 * | Action | Decision | Why |
 * |---|---|---|
 * | `/GoTo` (array or named destination) | `internal` | Navigating inside the open document is the viewer's own job. |
 * | `/URI` `http:`/`https:` | `external` (prompt) | ADR-P0020: no content acts without the user, and a prompt that shows the **whole** destination is the prompt. |
 * | `/URI` any other scheme | `blocked` | `javascript:`, `data:`, `file:`, `mailto:` — the scheme check is here, in the viewer, because the host port is narrowed to `http(s)` and "the host refused" must not be the only defence. |
 * | `/Launch` | `blocked` | ADR-P0020 names it first. A launch is a process start, an arbitrary local file, and frequently a payload; no prompt makes it safe. |
 * | `/GoToR` | `blocked` | ADR-P0020 names it: a remote *document* is an external fetch by another name. |
 * | `/SubmitForm`, `/ImportData` | `blocked` | ADR-P0020 names them. A read-only viewer has no form to submit. |
 * | JavaScript (`/JS`, `/JavaScript`) | `blocked` | ADR-P0020: document JavaScript is off by default and SL-5.JS.01 is years away. |
 * | unrecognised action, no action | `blocked` | Unknown is not "probably fine". |
 *
 * ## Why "blocked" is a *decision* and not an absence
 *
 * The failure this file exists to prevent is the quiet one: a link the viewer
 * does not understand, clicked, doing nothing, with no explanation. So
 * `blocked` carries a **reason** and the controller announces it in a live
 * region. A reader who clicks a `/Launch` link learns that this viewer will not
 * run it — a different and much better experience from a link that silently
 * swallows the click, and the difference between "active content is off by
 * default" (ADR-P0020) being true and being merely intended.
 *
 * ## Why the check is here and not in the host
 *
 * `WindowPort.openExternal` is narrowed to `http(s)` at the port
 * (`adapter.ts`), and the extension narrows it again. Two defences are not
 * redundant: the port's is a *type* boundary a future host could legitimately
 * widen (a desktop shell can hand `mailto:` to the OS), and this one is the
 * *policy*, which does not change with the host. If the only check lived at the
 * port, widening it for one shell would widen it for all — which is exactly how
 * `/Launch` gets re-enabled by accident.
 */

import type { LinkAction, LinkActionKind, PdfDestination } from "../platform/types.js";

/** Why a link was refused. Surfaced to the reader, never swallowed. */
export type LinkRefusal =
	/** `/Launch`: a process start or an arbitrary local file. */
	| "launch"
	/** `/GoToR`: navigating to another document. */
	| "remote-destination"
	/** `/SubmitForm`. */
	| "submit-form"
	/** `/ImportData`. */
	| "import-data"
	/** Document JavaScript (`/JS`). */
	| "javascript"
	/** A URI whose scheme is not `http`/`https`. */
	| "scheme"
	/** The action named a destination the document does not define. */
	| "missing-destination"
	/** `/URI` with no URI at all, or no action to run. */
	| "no-target"
	/** An action class the viewer does not implement. */
	| "unsupported";

/**
 * What activating a link does.
 *
 * A discriminated union rather than a set of booleans, because the interesting
 * failure mode is a state like `{ isExternal: true, isBlocked: true }`. Exactly
 * one of the three cases holds, always, and the compiler says so.
 */
export type LinkDecision =
	/** Navigate inside this document. */
	| { readonly kind: "internal"; readonly destination: PdfDestination }
	/**
	 * Ask the reader first. The URL is **verbatim** — the prompt shows exactly
	 * what the document said, including its query string, because a shortened
	 * or normalised destination is how a reader approves one URL and is sent to
	 * another.
	 */
	| { readonly kind: "external"; readonly url: string; readonly host: string }
	/** Refuse, with a reason the reader can be told. */
	| { readonly kind: "blocked"; readonly reason: LinkRefusal; readonly action: LinkActionKind };

/**
 * The schemes the viewer will open outside the app.
 *
 * `http:` and `https:` only, matching `WindowPort.openExternal`. `mailto:` and
 * `tel:` are deliberately absent: a desktop shell may legitimately hand those to
 * the OS, but "the viewer never does" is a promise this product can keep on
 * every host, and adding a scheme should be a one-line change someone can then
 * argue for in review.
 */
export const ALLOWED_EXTERNAL_SCHEMES: readonly string[] = ["http:", "https:"];

/**
 * The scheme of a URI, lowercased, or `null` when there is not one.
 *
 * Written by hand rather than with `URL` on purpose. `new URL()` throws on a
 * relative reference, normalises the host to lowercase and the path to
 * percent-encoding, and — the real problem — is a *browser* API: the
 * `platform-globals` lint gate forbids naming it in `apps/ui`, and rightly,
 * since the viewer's decision must be identical in Node, in a Worker and in a
 * test.
 *
 * A scheme is `[a-z][a-z0-9+.-]*` before the first `:` (RFC 3986 §3.1). A
 * relative reference has none, which is the case a document writes as a
 * `/Launch` with a relative file spec.
 */
export function schemeOf(uri: string): string | null {
	const colon = uri.indexOf(":");
	if (colon <= 0) {
		return null;
	}
	const head = uri.slice(0, colon);
	if (!/^[A-Za-z][A-Za-z0-9+.-]*$/.test(head)) {
		return null;
	}
	return `${head.toLowerCase()}:`;
}

/** True when the viewer may prompt to open this URI. */
export function isAllowedExternalScheme(uri: string): boolean {
	const scheme = schemeOf(uri);
	return scheme !== null && ALLOWED_EXTERNAL_SCHEMES.includes(scheme);
}

/**
 * The host shown in the prompt, or `null` when there is not one.
 *
 * Extracted textually for the same reason as {@link schemeOf}: the prompt must
 * show a value the reader can check, and one produced by a different parser in
 * a different host is a value the reader cannot check either. A URI whose
 * authority is empty (`http:///path`) yields `null`, and the caller then shows
 * the URL alone — the honest thing to show for a malformed URI.
 */
export function hostOf(uri: string): string | null {
	const match = /^[A-Za-z][A-Za-z0-9+.-]*:\/\/([^/?#]*)/.exec(uri);
	const authority = match?.[1] ?? "";
	// Strip userinfo: the prompt shows the host, and `evil.com@good.com` read
	// left-to-right is the oldest trick in the book.
	const afterAt = authority.includes("@")
		? authority.slice(authority.lastIndexOf("@") + 1)
		: authority;
	if (afterAt.length === 0) {
		return null;
	}
	// Bracketed IPv6 literal, with or without a port.
	const bracketed = /^\[([^\]]*)\](?::\d+)?$/.exec(afterAt);
	if (bracketed !== null) {
		return `[${bracketed[1] ?? ""}]`;
	}
	return afterAt.split(":")[0] ?? null;
}

/** The reason each disabled action class is refused, in one place. */
export const REFUSAL_FOR_ACTION: Readonly<Record<string, LinkRefusal>> = {
	launch: "launch",
	goToR: "remote-destination",
	submitForm: "submit-form",
	importData: "import-data",
	javascript: "javascript",
};

/**
 * Decide what activating `action` does. Pure and total.
 *
 * `resolveName` turns a named destination into a page; pass `undefined` and every
 * named destination resolves to "missing", which is the correct answer for a
 * document whose name tree the transport could not read. The function never
 * throws, and never returns a "maybe".
 *
 * The order is load-bearing: the ADR-P0020 classes are checked **before** any
 * field is examined. A `/Launch` action that also carries a `/URI`-shaped string
 * is a launch, and checking the string first would be the one bug in this file
 * that could re-enable active content.
 */
export function decideLinkAction(
	action: LinkAction,
	resolveName?: (name: string) => PdfDestination | undefined,
): LinkDecision {
	const refused = REFUSAL_FOR_ACTION[action.kind];
	if (refused !== undefined) {
		return { kind: "blocked", reason: refused, action: action.kind };
	}
	switch (action.kind) {
		case "goTo": {
			if (action.destination !== undefined) {
				return { kind: "internal", destination: action.destination };
			}
			const resolved = resolveName?.(action.name ?? "");
			return resolved === undefined
				? { kind: "blocked", reason: "missing-destination", action: action.kind }
				: { kind: "internal", destination: resolved };
		}
		case "named": {
			const resolved = resolveName?.(action.name ?? "");
			return resolved === undefined
				? { kind: "blocked", reason: "missing-destination", action: action.kind }
				: { kind: "internal", destination: resolved };
		}
		case "uri": {
			const uri = action.uri;
			if (uri === undefined || uri.length === 0) {
				return { kind: "blocked", reason: "no-target", action: action.kind };
			}
			if (!isAllowedExternalScheme(uri)) {
				return { kind: "blocked", reason: "scheme", action: action.kind };
			}
			return { kind: "external", url: uri, host: hostOf(uri) ?? "" };
		}
		case "none":
			return { kind: "blocked", reason: "no-target", action: action.kind };
		default:
			return { kind: "blocked", reason: "unsupported", action: action.kind };
	}
}

/**
 * Where a destination places its page, in the two alignments the page list
 * understands.
 *
 * A `/XYZ` destination names a *point* on the page, and honouring it means
 * scrolling to a position, not to a page — but `goToPage`/`scrollTopForPage` are
 * the only two ways to move (UI.02's rule, and the reason it is a rule: "go to
 * page" must have exactly one definition). So the point is reduced to the page
 * and the alignment, and the recorded loss is that a mid-page destination shows
 * the top of the page. Scrolling to a position inside a page is a UI.07
 * question with a real answer, and inventing that offset here would be the
 * second definition this codebase refuses to have.
 *
 * `/FitH` and `/FitV` are page alignments; `/Fit`, `/FitR`, `/FitB` and an
 * unknown kind all mean "the page, as it is".
 */
export function alignmentForDestination(destination: PdfDestination): "start" | "centre" {
	return destination.kind === "fitV" ? "start" : "centre";
}
