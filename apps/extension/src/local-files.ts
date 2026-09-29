/**
 * SL-4.EXT.09 — the `file://` access flow, as pure decisions.
 *
 * A user opens a PDF from a folder on their own machine. What should happen is a
 * *permission* conversation, and the whole difficulty is that Chrome will not
 * hold that conversation for us.
 *
 * ## There is no prompt to ask with
 *
 * Chrome documents the local-file grant in one place: the extension's own
 * details page. "If your extension needs to run on `file://` URLs… users must
 * give the extension access on its details page… to detect whether the user has
 * allowed access, you can call `extension.isAllowedFileSchemeAccess()`." The
 * match-pattern reference is blunter: `file:///` "requires the user to manually
 * grant access".
 *
 * So the grant is a **toggle in a browser page**, and
 * `chrome.permissions.request()` is not it. This module therefore never calls
 * it, and that is a decision with a reason rather than an omission: a runtime
 * request would either do nothing (Chrome ignores an origin it does not treat as
 * optional-grantable) or show a dialog that grants something the user cannot
 * then verify anywhere. The flow asks in the only way the platform allows — one
 * click to the page that has the switch, plus the written steps in case a
 * browser build does not surface the switch for an *optional* pattern — and it
 * detects the answer with the one API the documentation names. If a future
 * Chrome makes the `file` scheme requestable, this module is where that call
 * goes, and {@link FileAccess} already has the shape to receive it.
 *
 * ## Nothing intercepts, so nothing can report having failed
 *
 * This is what makes `file://` unlike every other pattern in `PATTERN_MATRIX`.
 * DNR evaluates a rule only against a request the extension is already entitled
 * to touch, so the `file` rule in `permissions.ts` is inert until the toggle is
 * on. Before the grant nothing is redirected, nothing fails and no error is
 * raised: Chrome's own PDF viewer opens the file and the extension is not told.
 * An interception-based design would have had nothing to hang a state on. So
 * detection is explicit — one boolean, asked for at the moment a local file is
 * in hand — and the states below are what it produces.
 *
 * ## Denial is a standing state, not an error
 *
 * There is no prompt, so there is no dismissal to catch and no "denied" event.
 * The real states are *not granted* and *granted but the read still failed*,
 * which need different sentences because they have different causes — and a
 * third that is not a state at all: a browser that cannot answer the question,
 * which must never be reported as a refusal.
 *
 * So the denial path is a defined {@link LocalFileOutcome} with words on the
 * page, never a spinner and never a thrown error — and, crucially, it is not a
 * dead end. `files.pickOpen` opens a local PDF **today**, with no permission at
 * all, because a `<input type="file">` needs none. The flow keeps that route on
 * screen in every state, which is the answer to "can this avoid a permission
 * change entirely": for opening a local file, yes, and it is offered first.
 * What the toggle buys is the *other* thing — the extension taking over when
 * the user navigates to a local PDF themselves, which no file picker can do.
 *
 * Pure: no `chrome.*`, no DOM, no globals. The browser half is
 * `ext/host-env.ts`, and the words are `viewer-strings.ts`.
 */


/**
 * The one name the user sees for the switch, in the browser's own words.
 *
 * Quoted verbatim from Chrome's UI ("Allow access to file URLs") so the page and
 * the browser's settings page agree, and exported so the string has one
 * definition: a page that invented its own name for the toggle would send a user
 * looking for something that is not there.
 */
export const FILE_ACCESS_TOGGLE = "Allow access to file URLs";

/** The browser page the toggle lives on. `chrome://` — linked, never loaded. */
export const EXTENSIONS_PAGE = "chrome://extensions/";

/** Chrome's extension-id shape, so a bad value never becomes a link. */
export function isExtensionId(value: string): boolean {
	return /^[a-p]{32}$/.test(value.trim());
}

/**
 * The deep link to this extension's own details page, where the toggle is.
 *
 * `chrome://extensions/?id=<id>` is the address Chrome itself uses when it says
 * "open this extension's details"; there is no API for it, because it is a page
 * the extension is *not* allowed to script. The query is built by concatenation
 * rather than through a `URL` object because `chrome://` has no JavaScript
 * origin grammar worth trusting here, and the only value interpolated is
 * Chrome's own 32-character extension id.
 *
 * An empty string for anything that is not an id is deliberate, and the check is
 * a pattern rather than an emptiness test. A link built as
 * `chrome://extensions/?id=` lands on a page listing *every* extension, which
 * reads as a broken deep link rather than as "this page could not be identified"
 * — so the caller is handed nothing to render and falls back to the written
 * steps.
 */
export function extensionDetailsUrl(extensionId: string): string {
	const id = extensionId.trim();
	return isExtensionId(id) ? `${EXTENSIONS_PAGE}?id=${id}` : "";
}


/**
 * Whether the user has granted the extension access to `file://` URLs.
 *
 * Three states, and the third is the one that matters:
 *
 * - `granted` — the toggle is on. The `file` rule is live and a `file://` read
 *   is allowed.
 * - `withheld` — the toggle is off. **This is the normal state, not a refusal
 *   and not an error**, and the copy is written accordingly: nothing has been
 *   changed, the file has not moved, and the browser can still open it.
 * - `unknown` — the browser could not answer. Chrome 99+ is the documented floor
 *   for `isAllowedFileSchemeAccess` and this extension's floor is 114, so this is
 *   a foreign browser or an API that has moved. It must never be collapsed into
 *   `withheld`: a page that says "you have not allowed this" when it simply could
 *   not check is making a claim about the user's browser that may be false.
 */
export type FileAccess = "granted" | "withheld" | "unknown";

/** How a local file's read ended. */
export type LocalRead =
	/** The bytes arrived. */
	| "ok"
	/**
	 * The read was attempted and failed. The only outcome that says anything
	 * about the *file* rather than the permission: the grant was on and the read
	 * still did not work.
	 */
	| "failed"
	/**
	 * The read was **not attempted**, because the grant was not there.
	 *
	 * A third state rather than a flavour of `failed`, and the distinction is the
	 * whole reason {@link readLocalFile} exists: "we did not try" and "we tried
	 * and it did not work" are different facts, and a page that showed the second
	 * sentence for the first one would be telling the user their file is broken
	 * when the extension never touched it. A test asserts the reader is not
	 * called at all in this state.
	 */
	| "skipped";

/**
 * What the page should do about a local file, as a closed set.
 *
 * One function over the whole state space, so the page has no decision of its
 * own to get wrong and the table is testable without a browser.
 */
export type LocalFileOutcome =
	/** Read it and open it. */
	| "open"
	/**
	 * The toggle is off. Explain, offer the deep link and the written steps, and
	 * keep the file-picker route on screen.
	 */
	| "request"
	/**
	 * The toggle is on and the read still failed. A *different* sentence, because
	 * the cause is different and permission copy would send the user to a switch
	 * that is already on.
	 */
	| "unreadable"
	/**
	 * The browser would not answer. Say that, and still offer both routes — a
	 * user whose browser cannot report the state is exactly the user most likely
	 * to need the written steps.
	 */
	| "unchecked";

/**
 * The outcome for one `(access, read)` pair.
 *
 * The precedence is the argument: a read that succeeded cannot have needed
 * permission, so `ok` is checked first and can never be overridden. Everything
 * else falls through to the access state, which is the whole point — the page
 * asks this instead of inferring a reason from a failed read and guessing.
 * `skipped` and `failed` deliberately land in the same place, because
 * {@link readLocalFile} only ever produces `skipped` when the access is not
 * `granted`, so the two are already known to agree by the time they get here.
 */
export function planLocalFileAccess(access: FileAccess, read: LocalRead): LocalFileOutcome {
	if (read === "ok") {
		return "open";
	}
	if (access === "granted") {
		return "unreadable";
	}
	return access === "unknown" ? "unchecked" : "request";
}


/**
 * The one place a `file://` read is decided, and the place that makes the
 * "never touched it" claim enforceable.
 *
 * The grant is asked about *first*, and the reader is **not called at all**
 * unless it came back `granted`. That is the difference between this flow and
 * every other way a viewer meets a file it cannot read: an attempt would produce
 * a browser error naming no cause, and a page that then said "your file is
 * unreadable" would be guessing. Not attempting is what lets the page say what
 * it actually knows.
 *
 * `reader` is a parameter so a test can assert it was never called, which is the
 * only falsifiable form of the claim. A rejection from `reader` is `failed` and
 * nothing else: a `file://` read that throws is a missing file, a file locked by
 * another program, or a browser that withdrew the grant between the question and
 * the read, and the page's "unreadable" sentence covers all three honestly enough
 * not to be a lie.
 */
export async function readLocalFile(
	access: FileAccess,
	url: string,
	reader: (url: string) => Promise<ArrayBuffer>,
): Promise<LocalRead> {
	if (access !== "granted") {
		return "skipped";
	}
	try {
		await reader(url);
		return "ok";
	} catch {
		return "failed";
	}
}


/** A `?src=` value, classified. */
export type SourceKind = "web" | "local" | "unsupported";

/** One `?src=` value, classified and normalised. */
export interface ClassifiedSource {
	readonly kind: SourceKind;
	/** The parsed URL, re-serialised. Never a string the caller splices anywhere. */
	readonly url: string;
	/** The scheme, for the sentence that names it. */
	readonly scheme: string;
}

/**
 * Classify a document URL the viewer was asked to open.
 *
 * Three classes, and the middle one is this task:
 *
 * - `web` — `http`/`https`. EXT.02's case, unchanged.
 * - `local` — `file`. The caller must ask {@link FileAccess} before reading,
 *   because a read without the grant fails in a way that names no cause.
 * - `unsupported` — `blob:`, `data:`, `wss:` and the rest. Refused by name, as
 *   before: they are not main-frame navigations, nothing intercepts them, and a
 *   page that tried would be reading state the extension was never given.
 *
 * `null` for a value that is not a URL at all, which is a *different* failure
 * from a URL with an unsupported scheme and gets a different sentence.
 */
export function classifySourceUrl(raw: string): ClassifiedSource | null {
	let parsed: URL;
	try {
		parsed = new URL(raw);
	} catch {
		return null;
	}
	if (parsed.protocol === "http:" || parsed.protocol === "https:") {
		return { kind: "web", url: parsed.toString(), scheme: parsed.protocol };
	}
	if (parsed.protocol === "file:") {
		return { kind: "local", url: parsed.toString(), scheme: parsed.protocol };
	}
	return { kind: "unsupported", url: parsed.toString(), scheme: parsed.protocol };
}

/** Is this a `file://` URL? The one-line form, for the places that only branch. */
export function isLocalFileUrl(raw: string): boolean {
	try {
		return new URL(raw).protocol === "file:";
	} catch {
		return false;
	}
}
