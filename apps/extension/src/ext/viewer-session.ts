/**
 * SL-4.EXT.06 - the viewer page's composition root.
 *
 * `apps/ui` receives an adapter; something has to decide *which* adapter, build
 * the pieces it needs, and tell the page what happened. That is this module,
 * and it is the only place in the extension that knows the page exists. The
 * adapter, the transport and the surface are all constructed from parameters
 * and are testable without a DOM; this is the seam where the real globals meet
 * them.
 *
 * ## The startup order, and why it is that order
 *
 * 1. Build the host env (reads globals, all lazily).
 * 2. Ask the service worker to make sure an offscreen document exists. This
 *    happens *before* the port connects, because `chrome.runtime.connect` does
 *    not queue: connect to a host that is not listening and the request that
 *    follows is simply lost. The message carries no document bytes - it is a
 *    verb, and that is what keeps document bytes out of the worker.
 * 3. Connect the port, build the adapter.
 * 4. Restore the stored telemetry opt-in, so a user who opted in last week is
 *    not asked again this week.
 *
 * ## Why this task ships no page list
 *
 * The plan's architecture table says the extension reuses `apps/web/ui` in a
 * "no-network" configuration, and the seam that makes that possible is this
 * adapter. The virtualised page list, the zoom ladder and the keyboard map are
 * UI.04+ and they mount against whatever `PageList` a later task composes;
 * shipping them here would put a half-built viewer in a size-budgeted package
 * and would collide with that work. `REUSE.md` records this explicitly.
 */

import type { PlatformAdapter } from "../../../ui/src/platform/adapter.js";
import { AdapterError, isAdapterError } from "../../../ui/src/platform/errors.js";
import type { DocHandle, DocumentSourceDescriptor } from "../../../ui/src/platform/types.js";
import { createExtensionAdapter, restoreTelemetryPreference } from "./adapter.js";
import { createPortLink } from "./engine-link.js";
import { ENGINE_PORT_NAMES } from "./engine-protocol.js";
import { type HostEnv, createBrowserHostEnv } from "./host-env.js";

/** A viewer session: the adapter, and the engine host it is talking to. */
export interface ViewerSession {
	readonly adapter: PlatformAdapter;
	readonly env: HostEnv;
}

/**
 * Bring up the engine host, the port and the adapter.
 *
 * `env` is a parameter so a test can supply one; the browser path is
 * {@link createBrowserViewerSession}, which is the only caller that passes
 * `createBrowserHostEnv()`.
 */
export async function createViewerSession(env: HostEnv): Promise<ViewerSession> {
	// 2. The worker owns the "is there already an offscreen document?" question,
	//    because only the worker can answer it without racing itself.
	await env.ensureEngineHost();

	// 3. Connect after the host exists. `chrome.runtime.connect` to a context
	//    that is not listening yet drops the first messages silently, which
	//    looks exactly like a hung engine.
	const link = createPortLink(env.runtime.connect(ENGINE_PORT_NAMES.request));
	const adapter = createExtensionAdapter({ env, link });

	// 4.
	await restoreTelemetryPreference({ env, adapter });

	return { adapter, env };
}

/** The browser's session, for `viewer-boot.ts`. */
export function createBrowserViewerSession(): Promise<ViewerSession> {
	return createViewerSession(createBrowserHostEnv());
}

/** An opened document, and the title the page should carry. */
export interface OpenedDocument {
	readonly doc: DocHandle;
	readonly title: string;
}

/** Title format. The host decides format; this is the extension's choice. */
export function titleFor(documentName: string): string {
	const trimmed = documentName.trim();
	return trimmed.length === 0 ? "Selis PDF Viewer" : `${trimmed} - Selis PDF Viewer`;
}

/**
 * The descriptor's display name, or a neutral fallback.
 *
 * `DocumentSourceDescriptor` is a union and only two of its six members carry a
 * name; the others name a location instead. A viewer title is not the place to
 * invent a path, so anything without a name gets the generic one.
 */
export function sourceName(descriptor: DocumentSourceDescriptor): string {
	return descriptor.kind === "bytes" || descriptor.kind === "blob" ? descriptor.name : "";
}

/**
 * Open a document and title the page for it.
 *
 * A refusal is translated into plain language here, at the edge, because
 * `AdapterError` carries a registry code and a `docState` - the two things a UI
 * renders - and the message is what a user reads. The codes are not flattened
 * into prose: `describeFailure` keeps the code so the message can name it.
 */
export async function openDocument(
	adapter: PlatformAdapter,
	descriptor: DocumentSourceDescriptor,
): Promise<OpenedDocument> {
	const doc = await adapter.engine.open(descriptor);
	const title = titleFor(sourceName(descriptor));
	await adapter.window.setTitle(title);
	return { doc, title };
}

/** A failure, phrased for a person, with the registry code kept. */
export interface DescribedFailure {
	/** The registry code, so the UI can style by it and tests can assert it. */
	readonly code: number;
	/** `docState`, so the UI can say what happened to the document. */
	readonly docState: string;
	/** One sentence for the user. */
	readonly message: string;
}

/**
 * Turn any rejection into something renderable.
 *
 * Every failure an adapter can produce is an `AdapterError` with a code and a
 * `docState`; anything else is a bug in the shell, and is reported as such
 * rather than being flattened into the same sentence. The alternative - a
 * generic "something went wrong" - is how a user ends up believing their
 * document was damaged when it was never touched.
 */
export function describeFailure(error: unknown): DescribedFailure {
	if (isAdapterError(error)) {
		return {
			code: error.code,
			docState: error.docState,
			message: `${error.message} (code ${error.code}, document ${error.docState})`,
		};
	}
	return {
		code: AdapterError.badArgument("unexpected failure").code,
		docState: "Unchanged",
		message:
			error instanceof Error
				? `Selis hit an unexpected problem: ${error.message}`
				: "Selis hit an unexpected problem.",
	};
}
