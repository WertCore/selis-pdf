/**
 * SL-4.EXT.02 + SL-4.EXT.09 — the redirect target's side of the interception,
 * and the local-file flow that hangs off it.
 *
 * The DNR rule rewrites the browser's address bar to `viewer.html?src=<url>`, so
 * this module's job is to recover the document URL exactly as the browser sent
 * it and act on what kind of document it is. EXT.02 owns the web case: the URL
 * arriving intact, the browser's own credentials surviving the redirect, and the
 * document never being uploaded. EXT.09 owns the `file:` case, which is not a
 * variant of that one but a different shape of problem — see `local-files.ts`
 * for why there is no prompt to ask with and nothing to intercept before the
 * grant.
 *
 * ## Three destinations, one page
 *
 * | `?src=` | What the page does |
 * |---|---|
 * | `http(s)://…` | fetch with the browser's credentials, open it (EXT.02) |
 * | `file:///…` | ask about the grant, then open it or explain (EXT.09) |
 * | absent | offer the local-file panel: the toolbar popup's only job |
 *
 * Anything else is refused by name. The absent case used to render "use the
 * toolbar button" inside the toolbar button; it now renders the one thing a
 * reader who clicked the icon actually came for, which includes a route that
 * works with no permission at all.
 *
 * ## Every path ends in words
 *
 * `describeFailure` turns a rejection into a sentence with its registry code
 * kept; the local flow turns a state into a sentence chosen by
 * `planLocalFileAccess`. There is no path here that leaves a spinner, and none
 * that reports a cause the extension has not checked — the difference between
 * "we did not try" and "we tried and it failed" is carried all the way to the
 * screen.
 */

import type { PlatformAdapter } from "../../ui/src/platform/adapter.js";
import { type HostEnv, createBrowserHostEnv } from "./ext/host-env.js";
import {
	createBrowserViewerSession,
	describeFailure,
	openDocument,
	sourceName,
} from "./ext/viewer-session.js";
import {
	type ClassifiedSource,
	type FileAccess,
	type LocalFileOutcome,
	classifySourceUrl,
	planLocalFileAccess,
	readLocalFile,
} from "./local-files.js";
import { SRC_PARAM } from "./permissions.js";
import { TOGGLE_NAME, type ViewerMessageKey, viewerText } from "./viewer-strings.js";

/** Why a viewer session has no document, for the SL-4.UI.12 error state.
 *
 * The `message` is the English one and the page shows the catalogue's — the
 * pair is deliberate rather than redundant, so that a test can assert the
 * *reason* without asserting a sentence, and so a translator is never blocked on
 * a `new Error(...)` in a boot path. `sourceErrorKey` maps one to the other.
 */
export class ViewerSourceError extends Error {
	constructor(
		readonly reason: "missing-src" | "invalid-src" | "unsupported-scheme",
		message: string,
		/** The scheme to name, for `unsupported-scheme` only. */
		readonly scheme = "",
	) {
		super(message);
		this.name = "ViewerSourceError";
	}
}

/** The element ids `viewer.html` declares and this module drives. */
const IDS = {
	status: "selis-status",
	local: "selis-local",
	localHeading: "selis-local-heading",
	localState: "selis-local-state",
	localSettings: "selis-local-settings",
	localCheck: "selis-local-check",
	localChoose: "selis-local-choose",
	localForm: "selis-local-form",
	localUrl: "selis-local-url",
	localGo: "selis-local-go",
} as const;

/**
 * The document URL this viewer session should open, classified.
 *
 * Round-trips through `URLSearchParams`, so a document URL carrying `?`, `#` or
 * its own `&src=` comes back byte-identical to what the rule encoded.
 *
 * `file:` is accepted and classified rather than refused. That refusal was
 * EXT.02's correct answer when there was no local-file flow to point at; now
 * there is one, and the schemes still refused are `blob:`, `data:` and the rest —
 * nothing intercepts them, they are not navigations, and a page that fetched one
 * would be reading state the extension was never handed.
 */
export function documentUrlFromViewerLocation(search: string): ClassifiedSource {
	const raw = new URLSearchParams(search).get(SRC_PARAM);
	if (raw === null) {
		throw new ViewerSourceError("missing-src", "no document was named");
	}
	const parsed = classifySourceUrl(raw);
	if (parsed === null) {
		throw new ViewerSourceError("invalid-src", "the document address is not a URL");
	}
	if (parsed.kind === "unsupported") {
		throw new ViewerSourceError(
			"unsupported-scheme",
			`Selis cannot open a ${parsed.scheme} document`,
			parsed.scheme,
		);
	}
	return parsed;
}

/**
 * The fetch the viewer uses for an intercepted document.
 *
 * `credentials: "include"` is the load-bearing part: the redirect preserved the
 * browser's cookies and the original request's auth, and dropping the
 * credentials here would turn every SSO-gated and basic-auth PDF into a login
 * prompt. `redirect: "follow"` lets a 302 chain resolve, which is the one
 * redirect case the DNR rule cannot see (see PATTERN_MATRIX).
 *
 * A `file:` URL is fetched **without** credentials and with the referrer
 * suppressed. The scheme carries no cookies and has no origin to leak a referrer
 * to, and a local path is not a cross-origin secret worth announcing — so
 * passing the web options through unchanged would be two claims the request
 * cannot honour.
 *
 * The response is never uploaded or persisted here: it is returned to the caller
 * and handed to the engine in-process.
 */
export function fetchInterceptedDocument(url: string): Promise<Response> {
	const local = new URL(url).protocol === "file:";
	return fetch(url, {
		credentials: local ? "omit" : "include",
		redirect: "follow",
		referrerPolicy: local ? "no-referrer" : "strict-origin-when-cross-origin",
	});
}

/** The document's file name, for the title and the page count line.
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

/** The sentence for one local-file state, as a catalogue key. */
export function localStateKey(access: FileAccess): ViewerMessageKey {
	if (access === "granted") {
		return "viewer.local.status.on";
	}
	return access === "unknown" ? "viewer.local.status.unknown" : "viewer.local.status.off";
}

/**
 * The sentence for a `?src=` that could not be used at all.
 *
 * `missing-src` is `null` because it is not a failure: the toolbar popup is not
 * a broken viewer, and the panel's own heading is the sentence for it. The other
 * two are failures with a cause, and `unsupported-scheme` takes `{scheme}` so the
 * page names the scheme rather than saying "unsupported".
 */
export function sourceErrorKey(reason: ViewerSourceError["reason"]): ViewerMessageKey | null {
	if (reason === "missing-src") {
		return null;
	}
	return reason === "invalid-src"
		? "viewer.status.invalidSource"
		: "viewer.status.unsupportedScheme";
}

/** The sentence for the outcome of a read, or `null` when the read succeeded. */
export function localOutcomeKey(outcome: LocalFileOutcome): ViewerMessageKey | null {
	if (outcome === "open") {
		return null;
	}
	return outcome === "request"
		? "viewer.local.denied"
		: outcome === "unreadable"
			? "viewer.local.unreadable"
			: "viewer.local.unchecked";
}

/** Everything the viewer page talks to, injected so the flow is testable. */
export interface ViewerDeps {
	readonly env: HostEnv;
	/**
	 * Bring up the engine host and hand back an adapter.
	 *
	 * Lazy, and called at most once per page, because it creates the offscreen
	 * document. The local-file *explainer* must not pay for that: a reader who
	 * has not granted file access and clicks nothing should not have started an
	 * engine on their behalf.
	 */
	session(): Promise<PlatformAdapter>;
	/** Read a document's bytes. Defaults to {@link fetchInterceptedDocument}. */
	read(url: string): Promise<ArrayBuffer>;
}

/** Write one line of status into the page. */
function report(container: HTMLElement, message: string): void {
	const status = container.querySelector<HTMLElement>(`#${IDS.status}`);
	if (status !== null) {
		status.textContent = message;
	}
}

/** Fill every `data-i18n` key the page's skeleton declares. */
function fill(root: Element): void {
	for (const node of root.querySelectorAll<HTMLElement>("[data-i18n]")) {
		const key = node.dataset.i18n as ViewerMessageKey;
		node.textContent = viewerText(key, { toggle: TOGGLE_NAME });
	}
}

/**
 * The `file://` flow, as one function over the state space.
 *
 * Returns the state and the outcome rather than rendering them, so the order of
 * operations — ask, then *maybe* read, then say — is visible in one place and
 * testable with a fake env. The renderer below is then a lookup, not a
 * decision.
 *
 * `url` may be `""`, which is the "no local file in hand yet" case: the toolbar
 * popup. The grant is still asked about, because the panel's first job is to
 * tell the reader where they stand, and a panel that claimed nothing about the
 * grant until a local file was in hand would be the old silent failure with
 * nicer furniture. Nothing is read in that case regardless.
 */
export async function runLocalFlow(
	deps: ViewerDeps,
	url: string,
): Promise<{ readonly access: FileAccess; readonly outcome: LocalFileOutcome }> {
	// The host already answers `unknown` rather than throwing, and this catches
	// the second time for a `HostEnv` a test supplied or a future port that
	// rejects. A total function is what lets the page render a state for every
	// outcome instead of a `catch` that shows nothing.
	let access: FileAccess = "unknown";
	try {
		access = await deps.env.fileAccess();
	} catch {
		access = "unknown";
	}
	const read = url === "" ? "skipped" : await readLocalFile(access, url, deps.read);
	return { access, outcome: planLocalFileAccess(access, read) };
}

/** Show one state in the page's two live regions: the grant, then the outcome. */
function renderLocalState(
	container: HTMLElement,
	state: { access: FileAccess; outcome: LocalFileOutcome },
): void {
	const state_line = container.querySelector<HTMLElement>(`#${IDS.localState}`);
	if (state_line !== null) {
		state_line.textContent = viewerText(localStateKey(state.access));
	}
	const outcome = localOutcomeKey(state.outcome);
	if (outcome !== null) {
		report(container, viewerText(outcome));
	}
}

/** The panel, revealed. Every local-file state is this panel plus a line of text. */
function showLocalPanel(root: Element): HTMLElement | null {
	const panel = root.querySelector<HTMLElement>(`#${IDS.local}`);
	if (panel === null) {
		return null;
	}
	panel.hidden = false;
	return panel;
}

/** Open a local file the reader chose themselves. Needs no permission at all. */
async function openChosenFile(deps: ViewerDeps, container: HTMLElement): Promise<void> {
	try {
		const adapter = await deps.session();
		const [descriptor] = await adapter.files.pickOpen({ multiple: false });
		if (descriptor === undefined) {
			report(container, viewerText("viewer.local.nothingChosen"));
			return;
		}
		const { doc } = await openDocument(adapter, descriptor);
		// `sourceName` is the one place that knows which descriptor members carry a
		// name; re-deriving it here would be a second answer to the same question
		// and would have to be kept in step.
		report(
			container,
			viewerText("viewer.status.opened", { name: sourceName(descriptor), pages: doc.pageCount }),
		);
	} catch (error) {
		report(
			container,
			viewerText("viewer.status.failure", { reason: describeFailure(error).message }),
		);
	}
}

/**
 * The panel's three controls, wired to the flow.
 *
 * Each one is a re-run of the same function rather than a special case, which is
 * what makes "on grant it proceeds" true without a second code path: the reader
 * comes back from the browser's settings page, presses **Check again**, and the
 * same `runLocalFlow` now returns `open` because the one thing it reads has
 * changed. There is no polling and no timer — Chrome's toggle lives in another
 * window and this page cannot see it change, so a background loop would be a
 * spinner that lies.
 */
function mountLocalPanel(deps: ViewerDeps, container: HTMLElement, localUrl: string): void {
	const rerun = async (): Promise<void> => {
		report(container, viewerText("viewer.status.starting"));
		const state = await runLocalFlow(deps, localUrl);
		if (state.outcome === "open" && localUrl !== "") {
			await openDocumentAt(deps, container, localUrl);
			return;
		}
		renderLocalState(container, state);
	};

	const settings = container.querySelector<HTMLAnchorElement>(`#${IDS.localSettings}`);
	if (settings !== null) {
		// The link's target is this extension's own id on Chrome's page, which is
		// not known until the page runs — hence no `href` in the markup. It is set
		// here so the control is a real link: focusable, middle-clickable, and
		// announced as a link rather than as an unlabelled gesture.
		//
		// When there is no URL the control degrades to a plain button and the
		// written steps above it are the route. A link that silently did nothing
		// would be worse than no link: the reader would be sent looking for a
		// switch with no way to reach it.
		const href = deps.env.extensionSettingsUrl();
		if (href === "") {
			settings.removeAttribute("href");
			settings.setAttribute("role", "button");
			settings.tabIndex = 0;
		} else {
			settings.href = href;
			settings.addEventListener("click", (event) => {
				// The host opens the page on this gesture, so the browser must not
				// open a second one. The `href` stays for keyboard activation and
				// for the middle-click a mouse user expects from a link.
				if (deps.env.openExtensionSettings() !== "") {
					event.preventDefault();
				}
			});
		}
	}
	container.querySelector(`#${IDS.localCheck}`)?.addEventListener("click", () => {
		void rerun();
	});
	container.querySelector(`#${IDS.localChoose}`)?.addEventListener("click", () => {
		void openChosenFile(deps, container);
	});
	container.querySelector(`#${IDS.localForm}`)?.addEventListener("submit", (event) => {
		event.preventDefault();
		const field = container.querySelector<HTMLInputElement>(`#${IDS.localUrl}`);
		const typed = field?.value.trim() ?? "";
		const classified = classifySourceUrl(typed);
		if (classified === null || classified.kind !== "local") {
			report(container, viewerText("viewer.local.addressInvalid", { value: typed }));
			return;
		}
		// The address bar is rewritten rather than the field being read again, so
		// the URL that was asked about and the URL on screen cannot disagree — the
		// same single-source rule EXT.02's `viewerUrlFor` exists for.
		const next = new URL(globalThis.location.href);
		next.searchParams.set(SRC_PARAM, classified.url);
		globalThis.location.assign(`${next.pathname}${next.search}`);
	});

	void rerun();
}

/** Open a document the viewer has the bytes for, and say what happened. */
async function openDocumentAt(
	deps: ViewerDeps,
	container: HTMLElement,
	url: string,
): Promise<void> {
	try {
		const bytes = await deps.read(url);
		const adapter = await deps.session();
		const { doc } = await openDocument(adapter, {
			kind: "bytes",
			bytes,
			name: fileNameFromUrl(url),
		});
		report(
			container,
			viewerText("viewer.status.opened", { name: fileNameFromUrl(url), pages: doc.pageCount }),
		);
	} catch (error) {
		report(
			container,
			viewerText("viewer.status.failure", { reason: describeFailure(error).message }),
		);
	}
}

/** The dependencies the page actually runs with. */
export function browserDeps(): ViewerDeps {
	let started: Promise<PlatformAdapter> | null = null;
	return {
		env: createBrowserHostEnv(),
		session() {
			started ??= createBrowserViewerSession().then((session) => session.adapter);
			return started;
		},
		async read(url) {
			const response = await fetchInterceptedDocument(url);
			if (!response.ok) {
				throw new Error(viewerText("viewer.status.serverError", { status: response.status }));
			}
			return await response.arrayBuffer();
		},
	};
}

/**
 * Bring the viewer page up: the document it was sent, or the local-file panel.
 *
 * Every failure path ends in the same place — a message in the page, with the
 * registry code kept — because a viewer that silently shows nothing is the one
 * failure mode the seam cannot excuse. The local-file paths end in the panel,
 * which is a *state* rather than a message: it stays on screen with its controls
 * live, so a reader who declines is looking at their next two options rather than
 * at a dead end.
 */
export function bootViewer(container: HTMLElement, deps: ViewerDeps = browserDeps()): void {
	fill(container);
	report(container, viewerText("viewer.status.starting"));

	let source: ClassifiedSource;
	try {
		source = documentUrlFromViewerLocation(globalThis.location.search);
	} catch (error) {
		// No document named, an address that is not a URL, or a scheme this
		// extension does not open. All three land on the local-file panel with the
		// reason on screen: for the first that panel is the whole answer, and for
		// the other two the reader has something to correct. `missing-src` gets no
		// line of its own — it is not a failure, it is what the toolbar button is.
		showLocalPanel(container);
		if (error instanceof ViewerSourceError) {
			const key = sourceErrorKey(error.reason);
			if (key !== null) {
				report(container, viewerText(key, { scheme: error.scheme }));
			}
		}
		mountLocalPanel(deps, container, "");
		return;
	}

	if (source.kind === "local") {
		showLocalPanel(container);
		mountLocalPanel(deps, container, source.url);
		return;
	}

	void openDocumentAt(deps, container, source.url);
}

/** Run the viewer when this module is loaded as the page's entry point. */
if (typeof document !== "undefined") {
	const mount = document.getElementById("selis-viewer");
	if (mount !== null) {
		bootViewer(mount);
	}
}
