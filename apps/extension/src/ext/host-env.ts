/**
 * SL-4.EXT.06 — the platform globals the extension shell is allowed to touch,
 * behind one injectable interface.
 *
 * ## Why this module exists
 *
 * `apps/ui` is forbidden from touching platform globals (`platform-globals.test.ts`),
 * so somebody has to. In the web app that somebody is `apps/web/host`'s entry
 * point; in the extension it is this package. Putting the globals behind
 * {@link HostEnv} is what lets `adapter.ts` be constructed in a Node test with
 * no `chrome` and no DOM — the same reason `viewer/surface.ts` exists on the UI
 * side of the seam. Composition (which env, which engine) belongs to
 * `viewer-session.ts` and `offscreen.js`, not to the adapter.
 *
 * ## The permission argument, stated once
 *
 * Every method here is reachable **without** adding a permission to the
 * EXT.01-approved set (`declarativeNetRequest`, `offscreen`, empty
 * `host_permissions`). That is a hard constraint on this task, and it is why
 * some ports are refused rather than implemented:
 *
 * - `chrome.storage` would need `"storage"`, which EXT.01 did not approve, so
 *   {@link HostEnv.storage} is backed by `localStorage` on the extension origin
 *   (`chrome-extension://<id>`), which is per-extension, persistent, and
 *   permission-free. It is a *different* store from the web app's and does not
 *   try to be.
 * - `chrome.offscreen.createDocument` may only be called from the service
 *   worker, so the viewer page asks for it over {@link HostEnv.ensureEngineHost}
 *   with a message that carries no document bytes. This is the one thing that
 *   crosses the service worker, and it is a verb, not a payload.
 * - Clipboard *read* needs `"clipboardRead"`, so `clipboardRead` is a capability
 *   the extension reports as `false` and the adapter refuses.
 */

import type { PickOpenOptions } from "../../../ui/src/platform/adapter.js";
import type { DocumentSourceDescriptor } from "../../../ui/src/platform/types.js";
import { ENSURE_ENGINE_HOST } from "./engine-protocol.js";
import { type FileAccess, extensionDetailsUrl } from "../local-files.js";

/** The subset of a `chrome.runtime` port this package uses. */
export interface HostPort {
	/** Send one message. Serialised as JSON, not structured-cloned. */
	postMessage(message: unknown): void;
	/** Tear the link down. */
	disconnect(): void;
	/** Replies arriving from the far end. Returns an unsubscribe. */
	onMessage(listener: (message: unknown) => void): () => void;
	/** The far end going away (offscreen document closed, worker killed). */
	onDisconnect(listener: () => void): () => void;
}

/** The subset of `chrome.runtime` this package uses. */
export interface HostRuntime {
	/** Open a port to another extension context by name. */
	connect(name: string): HostPort;
	/** One-shot message to the service worker; resolves with its reply. */
	sendToServiceWorker(message: unknown): Promise<unknown>;
}

/** Key/value persistence on the extension origin. */
export interface HostStorage {
	get(key: string): Promise<string | null>;
	set(key: string, value: string): Promise<void>;
	remove(key: string): Promise<void>;
}

/** The host capabilities `adapter.ts` composes its ports from. */
export interface HostEnv {
	readonly runtime: HostRuntime;
	readonly storage: HostStorage;
	/** Set the page/tab title. */
	setTitle(title: string): void;
	/**
	 * Hand a URL to the browser to open in a normal tab.
	 *
	 * The adapter has already reduced this to `http(s)`; the host does not
	 * re-derive policy, it just performs it. Deliberately *not*
	 * `chrome.tabs.create`: `tabs` is on the EXT.01 denied list, and an
	 * extension page may open a tab with `window.open` without it.
	 */
	openExternal(url: string): void;
	/** Open the print dialog for this page. */
	print(): void;
	/** Write text to the system clipboard (a user gesture is required). */
	writeClipboardText(text: string): Promise<void>;
	/** Show the file picker and resolve to the chosen files. */
	pickFiles(options: PickOpenOptions): Promise<readonly DocumentSourceDescriptor[]>;
	/**
	 * Ask the service worker to make sure an offscreen engine document exists.
	 *
	 * Resolves once `offscreen.html` is running. Safe to call repeatedly: the
	 * worker owns the "is there already one?" question, because only it can
	 * answer it without racing itself.
	 */
	ensureEngineHost(): Promise<void>;
	/**
	 * Has the user granted this extension access to `file://` URLs?
	 *
	 * `chrome.extension.isAllowedFileSchemeAccess()` and nothing else — no
	 * permission is required to *read* the grant, which is what makes this the
	 * whole of SL-4.EXT.09's detection story. A rejection resolves to `unknown`
	 * rather than throwing, and the viewer's copy for `unknown` is a different
	 * sentence from the copy for `withheld`: a browser that could not answer is
	 * not a browser that said no. See `src/local-files.ts`.
	 */
	fileAccess(): Promise<FileAccess>;
	/**
	 * The browser's details page for this extension, or `""` when there is not one
	 * to name.
	 *
	 * Separate from {@link openExtensionSettings} because the page needs the URL
	 * *before* any click: it is what turns the control into a real link rather
	 * than a gesture with a label. A method that opened the page could not answer
	 * that question without opening a tab to ask it.
	 */
	extensionSettingsUrl(): string;
	/**
	 * Open the browser's details page for this extension, where the
	 * "Allow access to file URLs" switch is, and report the URL it opened.
	 *
	 * Returns `""` when there is no such page to open — an unpacked extension with
	 * no `chrome.runtime.id`, or a browser that hides it. The caller needs to know
	 * so it can suppress the browser's own navigation and leave the **written
	 * steps** as the way through: a deep link that silently did nothing would be
	 * worse than no deep link, because the reader would be told to look for a
	 * switch with no way to reach it.
	 *
	 * `window.open` and **not** `chrome.tabs.create`, for the reason
	 * {@link openExternal} gives: `tabs` is on the EXT.01 denied list, and a
	 * reviewer's eye reads the API as a reach this extension does not have. The
	 * URL is not a parameter — this is the only page the host will ever open, so
	 * the host builds it and nothing can point it elsewhere.
	 */
	openExtensionSettings(): string;
	/** Monotonic milliseconds, for diagnostics. */
	now(): number;
}

/**
 * The real environment, read off the current window.
 *
 * Only `viewer-session.ts` calls this, and only in a packaged extension page —
 * so the globals are read here and nowhere else in the reuse path. Every
 * accessor is lazy: a module that reads `chrome` at import time cannot be
 * imported by a test, and `bundle.test.ts` imports the gate's own modules from
 * a plain Node process.
 */
export function createBrowserHostEnv(): HostEnv {
	const runtime = (globalThis as { chrome?: { runtime?: unknown } }).chrome?.runtime as
		| {
				connect?: (name: string) => HostPort;
				sendMessage?: (message: unknown) => Promise<unknown>;
		  }
		| undefined;

	return {
		runtime: {
			connect(name) {
				if (runtime?.connect === undefined) {
					throw new Error("chrome.runtime.connect is unavailable in this context");
				}
				return runtime.connect(name);
			},
			async sendToServiceWorker(message) {
				if (runtime?.sendMessage === undefined) {
					throw new Error("chrome.runtime.sendMessage is unavailable in this context");
				}
				return await runtime.sendMessage(message);
			},
		},

		storage: {
			async get(key) {
				return globalThis.localStorage.getItem(key);
			},
			async set(key, value) {
				globalThis.localStorage.setItem(key, value);
			},
			async remove(key) {
				globalThis.localStorage.removeItem(key);
			},
		},

		setTitle(title) {
			globalThis.document.title = title;
		},

		openExternal(url) {
			globalThis.open(url, "_blank", "noopener,noreferrer");
		},

		print() {
			globalThis.print();
		},

		async writeClipboardText(text) {
			await globalThis.navigator.clipboard.writeText(text);
		},

		async pickFiles(options) {
			return await pickFilesWithInput(options);
		},

		async ensureEngineHost() {
			await this.runtime.sendToServiceWorker({ type: ENSURE_ENGINE_HOST });
		},

		async fileAccess() {
			// SL-4.EXT.09. `chrome.extension.isAllowedFileSchemeAccess` is the
			// documented way to read the `file://` grant, it has existed since
			// Chrome 99 (this extension's floor is 114), and reading it needs no
			// permission — which is why the local-file flow detects the grant at
			// all without asking for anything first.
			//
			// `unknown` on anything unexpected, including a browser with no
			// `chrome.extension` at all. The viewer says something different about
			// that than about a refusal, on purpose.
			const api = (globalThis as { chrome?: { extension?: unknown } }).chrome?.extension as
				| { isAllowedFileSchemeAccess?: () => Promise<boolean> }
				| undefined;
			if (typeof api?.isAllowedFileSchemeAccess !== "function") {
				return "unknown";
			}
			try {
				return (await api.isAllowedFileSchemeAccess()) ? "granted" : "withheld";
			} catch {
				return "unknown";
			}
		},

		extensionSettingsUrl() {
			const id = (globalThis as { chrome?: { runtime?: { id?: unknown } } }).chrome?.runtime?.id;
			return extensionDetailsUrl(typeof id === "string" ? id : "");
		},

		openExtensionSettings() {
			const url = this.extensionSettingsUrl();
			if (url === "") {
				return "";
			}
			globalThis.open(url, "_blank", "noopener,noreferrer");
			return url;
		},

		now() {
			return globalThis.performance.now();
		},
	};
}

/**
 * `pickOpen` without a host permission: a transient `<input type="file">`.
 *
 * A file input needs no MV3 permission and no `userScripts`; the browser shows
 * the picker because of the user gesture that got here. The input is created,
 * used once and discarded — a long-lived hidden input is a dialog that can
 * never be re-armed, and would keep a picked `File` alive for the page's life.
 *
 * The result is a `blob` descriptor: the `File` itself, unread. Reading it is
 * `adapter.ts`'s job, on the same tick as `open`, so the descriptor never has
 * to be serialisable.
 */
async function pickFilesWithInput(
	options: PickOpenOptions,
): Promise<readonly DocumentSourceDescriptor[]> {
	const input = globalThis.document.createElement("input");
	input.type = "file";
	input.multiple = options.multiple === true;
	input.accept = (options.accept ?? [".pdf", "application/pdf"]).join(",");
	input.style.display = "none";
	globalThis.document.body.append(input);

	try {
		const files = await new Promise<FileList>((resolve, reject) => {
			input.addEventListener("change", () => resolve(input.files ?? new FileList()), {
				once: true,
			});
			// `cancel` is not universal; the focus fallback covers the rest.
			input.addEventListener(
				"cancel",
				() => reject(new DOMException("the file picker was dismissed", "AbortError")),
				{ once: true },
			);
			input.click();
		});

		const descriptors: DocumentSourceDescriptor[] = [];
		for (const file of files) {
			descriptors.push({ kind: "blob", blob: file, name: file.name });
		}
		return descriptors;
	} finally {
		input.remove();
	}
}
