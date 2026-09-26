/**
 * Cross-origin-isolation detection and the single-threaded fallback
 * (SL-4.WEB.01 DoD leg + SL-4.WASM.03 threading contract).
 *
 * The engine has two raster paths with identical output (ADR-P0004):
 * `threads` (SharedArrayBuffer + `wasm-bindgen-rayon`, needs COOP/COEP) and
 * `single` (same engine, one Worker, always available). The extension path
 * can never set COOP/COEP, and some embedding contexts strip the headers —
 * so "works with isolation disabled" is a release gate, not a nice-to-have.
 *
 * The UI reads the selected mode through `PlatformCapabilities.threads`
 * (SL-4.UI.01): `true` only when this module reports `threads`. No other
 * sniffing is allowed — the concrete browser adapter calls
 * {@link selectThreadMode} once per session and publishes the result.
 *
 * Pure functions with an injectable environment so the DoD test runs under
 * Vitest without browser globals.
 */

/** Minimal browser surface this module reads. All fields optional. */
export interface IsolationEnvironment {
	readonly crossOriginIsolated?: unknown;
	readonly sharedArrayBufferAvailable?: unknown;
}

/** Thread mode the engine should use. */
export type ThreadMode = "threads" | "single";

/**
 * Read `crossOriginIsolated` without touching globals when they are absent
 * (SSR, Vitest, workers without DOM). Anything non-`true` is `false`.
 */
export function isCrossOriginIsolated(env: IsolationEnvironment = {}): boolean {
	return env.crossOriginIsolated === true;
}

/**
 * Whether the threaded engine path is usable: isolated AND the
 * `SharedArrayBuffer` constructor is actually present. Both are required —
 * some browsers report isolation while SAB is still disabled by enterprise
 * policy, and treating that as threaded would crash the worker pool.
 */
export function canUseThreads(env: IsolationEnvironment = {}): boolean {
	return isCrossOriginIsolated(env) && env.sharedArrayBufferAvailable === true;
}

/**
 * Select the engine thread mode. Never throws, never requires isolation:
 * missing headers select `single`, which renders identically (slower).
 */
export function selectThreadMode(env: IsolationEnvironment = {}): ThreadMode {
	return canUseThreads(env) ? "threads" : "single";
}

/**
 * Read the live browser environment. Isolated from the pure selectors above
 * so the only impure function in this module is this one — and the only
 * place allowed to touch `globalThis`/`crossOriginIsolated` outside tests.
 */
export function readLiveEnvironment(): IsolationEnvironment {
	const g = globalThis as unknown as {
		crossOriginIsolated?: unknown;
		SharedArrayBuffer?: unknown;
	};
	return {
		crossOriginIsolated: g.crossOriginIsolated === true,
		sharedArrayBufferAvailable: typeof g.SharedArrayBuffer === "function",
	};
}

/** Select the thread mode for the live page (what the adapter publishes). */
export function selectLiveThreadMode(): ThreadMode {
	return selectThreadMode(readLiveEnvironment());
}
