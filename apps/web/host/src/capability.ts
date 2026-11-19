/**
 * Portable capability detection for the web host (SL-4.EXT.10).
 *
 * Firefox and Safari are first-class ports. This module is the single place
 * that asks "what can this browser actually do?" and every host feature reads
 * a typed result rather than sniffing a user agent or assuming a Chromium API.
 * Two rules make the port honest:
 *
 * 1. Detection is presence-only. It never *invokes* an API, so probing cannot
 *    open storage, create a worker, or throw on a browser that lacks the
 *    feature. Exotic getters are contained.
 * 2. Every capability has a documented fallback (`CAPABILITY_FALLBACKS`).
 *    Chromium-only surfaces — OPFS synchronous access handles, File System
 *    Access picker, `OffscreenCanvas` — are detected, never required: the host
 *    falls back to Blob/`<input type="file">`, the main-thread canvas, and the
 *    single-threaded engine respectively.
 *
 * The code here is portable by construction. It has NOT been run in Firefox
 * or Safari from this repository (no such browser is available in CI); the
 * tests cover the typed result and the no-invocation guarantee, not those
 * engines. Do not restate this as "tested in Firefox/Safari".
 */

/** A bag of globals. Tests inject one; production uses `globalThis`. */
export interface CapabilityScope {
	readonly [key: string]: unknown;
}

/** Typed result of one detection pass. Every field is a boolean, never "maybe". */
export interface BrowserCapabilities {
	readonly serviceWorker: boolean;
	readonly webAssembly: boolean;
	readonly opfs: boolean;
	readonly opfsSyncHandle: boolean;
	readonly fileSystemAccess: boolean;
	readonly offscreenCanvas: boolean;
	readonly indexedDb: boolean;
	readonly blobUrl: boolean;
	readonly webLocks: boolean;
	readonly structuredClone: boolean;
	readonly dialogModal: boolean;
	readonly requestAnimationFrame: boolean;
	readonly textEncoder: boolean;
	readonly resizeObserver: boolean;
	readonly intersectionObserver: boolean;
	readonly sharedArrayBuffer: boolean;
}

/** The names of every detected capability, for iterating in tests and logs. */
export const CAPABILITY_NAMES: readonly (keyof BrowserCapabilities)[] = [
	"serviceWorker",
	"webAssembly",
	"opfs",
	"opfsSyncHandle",
	"fileSystemAccess",
	"offscreenCanvas",
	"indexedDb",
	"blobUrl",
	"webLocks",
	"structuredClone",
	"dialogModal",
	"requestAnimationFrame",
	"textEncoder",
	"resizeObserver",
	"intersectionObserver",
	"sharedArrayBuffer",
] as const;

/**
 * What the host uses when a capability is absent. This is the promise that no
 * Chromium-only API is required: each entry names a real, portable fallback.
 */
export const CAPABILITY_FALLBACKS: Readonly<Record<keyof BrowserCapabilities, string>> = {
	serviceWorker: "no offline cache; the app still runs online from the local origin",
	webAssembly: "no local engine — refuse document processing with an explicit message",
	opfs: "keep the document in a Blob handle for this session only",
	opfsSyncHandle: "read the handle through the Worker's async Blob path",
	fileSystemAccess: "use <input type=file> and a Blob download instead of save-in-place",
	offscreenCanvas: "compose tiles on the main thread with a regular canvas",
	indexedDb: "keep session state in memory; no durable local cache",
	blobUrl: "download through a same-origin object-less Blob URL is impossible; refuse export",
	webLocks: "serialize per-document work with an in-memory lock",
	structuredClone: "copy plain messages explicitly before postMessage",
	dialogModal: "render an in-page panel with role=dialog and focus management",
	requestAnimationFrame: "schedule presentation with a timer",
	textEncoder: "use the host-provided byte codec",
	resizeObserver: "recalculate layout on the window resize event",
	intersectionObserver: "fall back to a throttled scroll handler",
	sharedArrayBuffer: "select the single-threaded engine (SL-4.WASM.03)",
};

function read(obj: unknown, key: string): unknown {
	if (obj === null || (typeof obj !== "object" && typeof obj !== "function")) {
		return undefined;
	}
	try {
		return (obj as Record<string, unknown>)[key];
	} catch {
		// A hostile getter must not turn detection into a page load failure.
		return undefined;
	}
}

function isFunction(value: unknown): boolean {
	return typeof value === "function";
}

function hasFunctionPath(scope: CapabilityScope, ...path: readonly string[]): boolean {
	let current: unknown = scope;
	for (const key of path) {
		current = read(current, key);
		if (current === undefined) {
			return false;
		}
	}
	return isFunction(current);
}

/**
 * Detect every capability once, without invoking any of them.
 *
 * `scope` is injectable so the whole matrix is testable in plain Node: pass an
 * object shaped like a browser's globals to get `true`, or `{}` to get the
 * all-`false` result a restricted embedder would see.
 */
export function detectCapabilities(scope: CapabilityScope = globalThis as CapabilityScope): BrowserCapabilities {
	const wasm = read(scope, "WebAssembly");
	const dialog = read(scope, "HTMLDialogElement");
	return {
		serviceWorker: hasFunctionPath(scope, "navigator", "serviceWorker", "register"),
		webAssembly: isFunction(read(wasm, "compile")),
		opfs: hasFunctionPath(scope, "navigator", "storage", "getDirectory"),
		opfsSyncHandle: isFunction(read(scope, "FileSystemSyncAccessHandle")),
		fileSystemAccess: isFunction(read(scope, "showOpenFilePicker")),
		offscreenCanvas: isFunction(read(scope, "OffscreenCanvas")),
		indexedDb: read(scope, "indexedDB") !== undefined,
		blobUrl: hasFunctionPath(scope, "URL", "createObjectURL"),
		webLocks: hasFunctionPath(scope, "navigator", "locks", "request"),
		structuredClone: isFunction(read(scope, "structuredClone")),
		dialogModal: isFunction(read(read(dialog, "prototype"), "showModal")),
		requestAnimationFrame: isFunction(read(scope, "requestAnimationFrame")),
		textEncoder: isFunction(read(scope, "TextEncoder")),
		resizeObserver: isFunction(read(scope, "ResizeObserver")),
		intersectionObserver: isFunction(read(scope, "IntersectionObserver")),
		sharedArrayBuffer: isFunction(read(scope, "SharedArrayBuffer")),
	};
}

/** Capabilities that are missing, paired with the fallback the host will use. */
export function missingWithFallbacks(
	caps: BrowserCapabilities,
): readonly { readonly capability: keyof BrowserCapabilities; readonly fallback: string }[] {
	return CAPABILITY_NAMES.filter((name) => !caps[name]).map((name) => ({
		capability: name,
		fallback: CAPABILITY_FALLBACKS[name],
	}));
}
