/**
 * SL-4.EXT.06 - the extension's `PlatformAdapter`.
 *
 * This is the object that lets `apps/ui` run inside the MV3 viewer unchanged.
 * The UI asks for an adapter at startup and never touches a platform global
 * (`platform-globals.test.ts`); everything host-specific is a decision made
 * here, once, where it can be read.
 *
 * ## What the extension supplies, and what it refuses
 *
 * | Port | Verdict | Why |
 * |---|---|---|
 * | `engine` | supplied | Offscreen document over a `chrome.runtime` port (`engine-client.ts`). |
 * | `files.pickOpen` | supplied | A transient `<input type="file">`. No MV3 permission needed. |
 * | `files.pickSave` | **refused** | Save-in-place is a File System Access handle. The viewer is read-only, and the only caller would be Phase 5. Refusing now keeps `fileSystemAccess: false` honest. |
 * | `storage` | supplied | `localStorage` on the extension origin. `chrome.storage` would need the `storage` permission, which EXT.01 did not approve. |
 * | `clipboard.writeText` | supplied | The async clipboard API, on a user gesture. |
 * | `clipboard.readText` | **refused** | Needs the `clipboardRead` permission. Not approved, so `clipboardRead: false`. |
 * | `print` | supplied | `window.print()`. |
 * | `telemetry` | supplied, inert | Opt-in flag is honoured and stored; there is no sink and no network, so `record` is a no-op whatever the flag says. |
 * | `window.setTitle` | supplied | `document.title`. |
 * | `window.openExternal` | supplied, narrowed | `http(s)` only - see below. |
 * | `window.onDeepLink` | **refused** | `deepLinks: false`; no host routes links in, and the options page that would is EXT.07. |
 *
 * ### Why `openExternal` is narrowed to `http(s)`
 *
 * ADR-P0020 disables document-driven navigation by default: `/Launch`,
 * `/GoToR`, `/SubmitForm` and `/ImportData` are off unless scripting is
 * explicitly enabled, and a launch must never be a silent side effect. The
 * `WindowPort` contract says the UI prompts with the full destination first
 * and this port only performs the host action - so the *scheme* check belongs
 * here, where a document cannot talk its way past it. `javascript:`, `data:`,
 * `file:` and every other scheme are refused by name rather than passed to
 * `window.open`, because a scheme that reaches `window.open` from a page
 * holding a document is exactly the case P0020 is about.
 *
 * ## Telemetry is inert, and that is a feature
 *
 * `record` is a no-op even when the user has opted in, because the extension
 * has no endpoint and no permission to reach one (ADR-P0017: opt-in, and no
 * event field can carry document data). An adapter that silently buffered
 * events "for later" would be a store waiting to be misused; one that drops
 * them cannot be.
 */

import type {
	ClipboardPort,
	FilePort,
	PlatformAdapter,
	PrintPort,
	StoragePort,
	TelemetryPort,
	WindowPort,
} from "../../../ui/src/platform/adapter.js";
import { AdapterError } from "../../../ui/src/platform/errors.js";
import type {
	DeepLink,
	DocumentSourceDescriptor,
	PlatformCapabilities,
	SaveTarget,
	TelemetryEvent,
} from "../../../ui/src/platform/types.js";
import { readSettings, writeTelemetryOptIn } from "../options-state.js";
import { createEnginePort } from "./engine-client.js";
import type { EngineLink } from "./engine-link.js";
import type { HostEnv } from "./host-env.js";

/** The extension's capability set. Fixed for the session, as the contract says. */
export const EXTENSION_CAPABILITIES: PlatformCapabilities = {
	platform: "extension",
	// A file input is a picker; it needs no permission and no host grant.
	filePickers: true,
	// Refused: see the table. Nothing in a read-only viewer asks for a handle.
	fileSystemAccess: false,
	// Refused: the engine's source vocabulary is "bytes a user handed us".
	opfs: false,
	// Unreachable: with `host_permissions: []` there is no document origin to
	// range-request. The intercepted URL is fetched once, in the page.
	httpRange: false,
	// Refused: `clipboardRead` is not in the EXT.01 approved permission set.
	clipboardRead: false,
	print: true,
	// `localStorage` on the extension origin survives sessions.
	persistedStorage: true,
	// An extension page is never cross-origin isolated, so no SharedArrayBuffer.
	threads: false,
	// Nothing routes deep links into the viewer; that is EXT.07's options page.
	deepLinks: false,
};

/** Build the extension adapter over `env`, with its engine reached via `link`. */
export function createExtensionAdapter(options: {
	env: HostEnv;
	link: EngineLink;
}): PlatformAdapter {
	const { env, link } = options;

	const storage: StoragePort = {
		get: (key) => env.storage.get(key),
		set: (key, value) => env.storage.set(key, value),
		delete: (key) => env.storage.remove(key),
	};

	const files: FilePort = {
		async pickOpen(pickOptions): Promise<readonly DocumentSourceDescriptor[]> {
			return await env.pickFiles(pickOptions ?? {});
		},
		async pickSave(): Promise<SaveTarget | null> {
			// Refused rather than stubbed. Returning `null` would read as "the
			// user cancelled", which is a lie a UI cannot tell apart from a real
			// cancellation and would report as a failed save.
			throw AdapterError.badArgument(
				"the extension viewer is read-only and cannot choose a save target",
			);
		},
	};

	const clipboard: ClipboardPort = {
		writeText: (text) => env.writeClipboardText(text),
		async readText(): Promise<string> {
			throw AdapterError.badArgument(
				"clipboard read is not available in the extension: it would need the 'clipboardRead' permission, which this extension does not request",
			);
		},
	};

	const print: PrintPort = {
		async print(): Promise<void> {
			env.print();
		},
	};

	let telemetryEnabled = false;

	const telemetry: TelemetryPort = {
		isEnabled: () => telemetryEnabled,
		async setEnabled(enabled: boolean): Promise<void> {
			telemetryEnabled = enabled;
			// SL-4.EXT.07: written through the options page's own writer, so the
			// key and the meaning of "on" have one definition between the page
			// that offers the choice and the adapter that honours it.
			await writeTelemetryOptIn(env.storage, enabled);
		},
		record(_event: TelemetryEvent): void {
			// Intentionally empty, enabled or not. See the module doc.
		},
	};

	const windowPort: WindowPort = {
		async setTitle(title: string): Promise<void> {
			env.setTitle(title);
		},
		async openExternal(url: string): Promise<void> {
			env.openExternal(requireExternalUrl(url));
		},
		onDeepLink(_handler: (link: DeepLink) => void): () => void {
			// Nothing can deliver a link, so the handler is never retained: a
			// closure held for the page's life would be a listener on nothing.
			return () => {};
		},
	};

	return {
		capabilities: EXTENSION_CAPABILITIES,
		engine: createEnginePort(link),
		files,
		storage,
		clipboard,
		print,
		telemetry,
		window: windowPort,
		async dispose(): Promise<void> {
			link.close();
		},
	};
}

/**
 * Restore the telemetry opt-in a previous session stored.
 *
 * Takes the env explicitly rather than casting the adapter to reach a private
 * field: the flag lives in the same `localStorage` the storage port writes, and
 * a function that had to introspect its own argument would be a worse version
 * of the direct `chrome.storage` call it exists to avoid.
 */
export async function restoreTelemetryPreference(options: {
	env: HostEnv;
	adapter: PlatformAdapter;
}): Promise<void> {
	const { telemetryOptIn } = await readSettings(options.env.storage);
	if (telemetryOptIn) {
		await options.adapter.telemetry.setEnabled(true);
	}
}

/**
 * The only URLs this extension will hand to the browser.
 *
 * A scheme allow-list, not a deny-list: a deny-list has to be updated every
 * time a scheme is invented, and the failure mode is handing a document's
 * `/Launch` target to the browser. `http`/`https` cover every legitimate
 * case (a help page, a citation, the "open in the web app" hand-off that
 * SL-4.EXT.08 will add).
 */
export function requireExternalUrl(url: string): string {
	let parsed: URL;
	try {
		parsed = new URL(url);
	} catch {
		throw AdapterError.badArgument(`'${url}' is not a URL this extension can open`);
	}
	if (parsed.protocol !== "https:" && parsed.protocol !== "http:") {
		throw AdapterError.badArgument(
			`the extension will not open a ${parsed.protocol} link - only http and https`,
		);
	}
	return parsed.toString();
}
