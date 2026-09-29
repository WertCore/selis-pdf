/**
 * SL-4.EXT.06 - a `HostEnv` that records instead of touching a browser, for
 * tests only. **Not on `PACKAGE_ENTRIES` and must not be.**
 *
 * The adapter's host ports are exactly the code that would call `chrome.*` and
 * the DOM, so they are the code that cannot be exercised in Node. This stands in
 * for them, and it *records* rather than performs: a test asserting that
 * `setTitle` reached the document is a test of this recorder, not of the
 * browser, and says so by being explicit about what it holds.
 *
 * The recording is the point. The refusals in `adapter.ts` are only worth
 * anything if a test can see that, say, `print` was never called when the
 * capability is off - which is a statement about a list of calls, not a return
 * value.
 */

import type { PickOpenOptions } from "../../../ui/src/platform/adapter.js";
import type { DocumentSourceDescriptor } from "../../../ui/src/platform/types.js";
import type { FileAccess } from "../local-files.js";
import type { HostEnv, HostPort, HostRuntime } from "./host-env.js";

/** Everything the fake env was asked to do. */
export interface HostRecording {
	readonly titles: string[];
	printed: number;
	readonly clipboardWrites: string[];
	readonly externalUrls: string[];
	readonly filePicks: PickOpenOptions[];
	/** How many times the page asked the worker for an engine document. */
	ensureEngineHostCalls: number;
	/** How many times the page asked whether the `file://` grant is on. */
	fileAccessCalls: number;
	/** The last `file://` grant the page was told about. */
	fileAccess: FileAccess;
	/** How many times the page asked to open the browser's details page. */
	settingsOpened: number;
	/**
	 * What `openExtensionSettings` hands back.
	 *
	 * `""` is a real case, not an error case: a browser that will not open
	 * `chrome://extensions` — or an unpacked extension with no id — leaves the flow
	 * to fall back on the written steps, and a test that cannot produce that state
	 * cannot check the fallback.
	 */
	settingsUrl: string;
}

/** A fake env plus the ability to steer what a `connect` returns. */
export interface FakeHostEnv {
	readonly env: HostEnv;
	readonly recording: HostRecording;
	/** Make the next `connect` hand out `port`. */
	usePort(port: HostPort): void;
	/** Make the next `pickOpen` resolve to these descriptors. */
	queueOpen(sources: readonly DocumentSourceDescriptor[]): void;
	/** Make `ensureEngineHost` reject, as a worker that cannot create one would. */
	failEngineHost(error: Error): void;
	/** Make `fileAccess` reject, as a browser without the API would. */
	failFileAccess(error: Error): void;
}

/** Build a fake env. */
export function createFakeHostEnv(): FakeHostEnv {
	const recording: HostRecording = {
		titles: [],
		printed: 0,
		clipboardWrites: [],
		externalUrls: [],
		filePicks: [],
		ensureEngineHostCalls: 0,
		fileAccessCalls: 0,
		fileAccess: "withheld",
		settingsOpened: 0,
		settingsUrl: "chrome://extensions/?id=abcdefghijklmnopabcdefghijklmnop",
	};
	const store = new Map<string, string>();
	let port: HostPort | null = null;
	let queued: readonly DocumentSourceDescriptor[] = [];
	let ensureFailure: Error | null = null;
	let fileAccessFailure: Error | null = null;

	const runtime: HostRuntime = {
		connect() {
			if (port === null) {
				throw new Error("the fake env has no port installed");
			}
			return port;
		},
		async sendToServiceWorker() {
			return { ok: true };
		},
	};

	const env: HostEnv = {
		runtime,
		storage: {
			async get(key) {
				return store.get(key) ?? null;
			},
			async set(key, value) {
				store.set(key, value);
			},
			async remove(key) {
				store.delete(key);
			},
		},
		setTitle(title) {
			recording.titles.push(title);
		},
		openExternal(url) {
			recording.externalUrls.push(url);
		},
		print() {
			recording.printed += 1;
		},
		async writeClipboardText(text) {
			recording.clipboardWrites.push(text);
		},
		async pickFiles(options) {
			recording.filePicks.push(options);
			return queued;
		},
		async ensureEngineHost() {
			recording.ensureEngineHostCalls += 1;
			if (ensureFailure !== null) {
				throw ensureFailure;
			}
		},
		async fileAccess() {
			recording.fileAccessCalls += 1;
			if (fileAccessFailure !== null) {
				throw fileAccessFailure;
			}
			return recording.fileAccess;
		},
		openExtensionSettings() {
			recording.settingsOpened += 1;
			return recording.settingsUrl;
		},
		extensionSettingsUrl() {
			return recording.settingsUrl;
		},
		now() {
			return 0;
		},
	};

	return {
		env,
		recording,
		usePort(next) {
			port = next;
		},
		queueOpen(sources) {
			queued = sources;
		},
		failEngineHost(error) {
			ensureFailure = error;
		},
		failFileAccess(error) {
			fileAccessFailure = error;
		},
	};
}
