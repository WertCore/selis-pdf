/**
 * Registering the service worker (SL-4.WEB.02, requirement 1).
 *
 * Three things this module decides, each of which is a place a naive
 * registration goes wrong:
 *
 * - **Where.** `/sw.js` at the site root, scope `/`. The app is served from the
 *   origin root and the Worker script has to sit at or above everything it
 *   controls, so a scope narrower than the app would leave the WASM chunks
 *   (`/wasm/…`) outside the worker and the offline story half-built.
 * - **Whether.** Registration is refused, loudly but non-fatally, when there is
 *   no `ServiceWorkerContainer`, when the context is not secure, or under
 *   `file://` (a service worker cannot register there at all). Refusing is a
 *   returned value, not a thrown error: the viewer must still open, view, and
 *   print a local file with no worker installed — offline capability is an
 *   enhancement over a shell that always works, never a precondition for one.
 * - **When an update lands.** `updateViaCache: "none"` so the browser always
 *   re-fetches the worker script rather than trusting a long-lived HTTP cache
 *   entry. The worker itself does not call `skipWaiting` (see `sw.ts`), so a new
 *   version waits; {@link watchForUpdate} reports it and {@link applyUpdate}
 *   takes it at a moment the page chooses — never mid-document.
 */

import { SERVICE_WORKER_PATH, SKIP_WAITING_MESSAGE, SW_SCOPE } from "./sw-policy.js";

/** The browser surface registration needs, injected so the tests can deny it. */
export interface RegisterEnvironment {
	readonly container?: {
		register(url: string, options?: RegistrationOptions): Promise<ServiceWorkerRegistration>;
	} | null;
	/** `window.isSecureContext`. */
	readonly secureContext?: boolean | undefined;
	/** `location.protocol`; `"file:"` can never host a service worker. */
	readonly protocol?: string | undefined;
}

/** Why registration did not happen. Each is an expected condition, not a failure. */
export type Unavailable = "unsupported" | "insecure-context" | "file-protocol" | "no-container";

export type RegisterResult =
	| { readonly ok: true; readonly scope: string; readonly registration: ServiceWorkerRegistration }
	| { readonly ok: false; readonly reason: Unavailable | "failed"; readonly detail?: string };

/** Read the live page's registration environment. */
export function liveRegisterEnvironment(): RegisterEnvironment {
	const scope = globalThis as unknown as {
		navigator?: { serviceWorker?: RegisterEnvironment["container"] };
		isSecureContext?: boolean;
		location?: { protocol?: string };
	};
	return {
		container: scope.navigator?.serviceWorker ?? null,
		secureContext: scope.isSecureContext,
		protocol: scope.location?.protocol,
	};
}

/**
 * Whether registration may be attempted, and why not when it may not.
 * Split out from the registration itself so the refusal conditions are testable
 * without a browser.
 */
export function registrationBlocker(env: RegisterEnvironment): Unavailable | null {
	if (env.container === undefined || env.container === null) {
		return "no-container";
	}
	if (env.protocol === "file:") {
		return "file-protocol";
	}
	if (env.secureContext === false) {
		return "insecure-context";
	}
	return null;
}

/** Registration options, spelled out so the update story is visible in one place. */
export function registrationOptions(): RegistrationOptions {
	return {
		scope: SW_SCOPE,
		// The worker is a module: it imports `./sw-policy.js`. `updateViaCache:
		// none` keeps a stale HTTP-cached `sw.js` from pinning an old version.
		type: "module",
		updateViaCache: "none",
	};
}

/**
 * Register the worker. Never throws: every failure — including a registration
 * that rejects — comes back as a value the caller can log and carry on from.
 */
export async function registerServiceWorker(
	env: RegisterEnvironment = liveRegisterEnvironment(),
): Promise<RegisterResult> {
	const blocked = registrationBlocker(env);
	if (blocked !== null) {
		return { ok: false, reason: blocked };
	}
	try {
		const registration = await (
			env.container as NonNullable<RegisterEnvironment["container"]>
		).register(SERVICE_WORKER_PATH, registrationOptions());
		return { ok: true, scope: registration.scope, registration };
	} catch (error) {
		return {
			ok: false,
			reason: "failed",
			detail: error instanceof Error ? error.message : String(error),
		};
	}
}

/** What {@link watchForUpdate} reports. */
export interface UpdateReport {
	/** A new worker is installed and waiting; the page decides when it takes over. */
	readonly kind: "update-ready";
	readonly registration: ServiceWorkerRegistration;
}

/**
 * Report an installed-but-waiting worker.
 *
 * The first install is not an update: with no controller there is nothing to
 * replace, and the worker is already active. Only a worker that installed
 * *alongside an existing controller* is an update, and that is the one worth
 * telling the shell about.
 */
export function watchForUpdate(
	registration: ServiceWorkerRegistration,
	onUpdate: (report: UpdateReport) => void,
): void {
	// `ServiceWorkerRegistration.controller` is in lib.dom; this package's
	// declaration set does not carry it, so the one field read here is declared
	// locally rather than casting the whole registration away.
	const hasController = (candidate: ServiceWorkerRegistration): boolean => {
		const controller = (candidate as unknown as { controller?: unknown }).controller;
		return controller !== null && controller !== undefined;
	};

	registration.addEventListener("updatefound", () => {
		const installing = registration.installing;
		if (installing === null) {
			return;
		}
		installing.addEventListener("statechange", () => {
			// A controller is only set once a worker already controls this page,
			// which is exactly the "replacement" case; the first install is not an
			// update and must not be announced as one.
			if (installing.state === "installed" && hasController(registration)) {
				onUpdate({ kind: "update-ready", registration });
			}
		});
	});
}

/**
 * Hand control to a waiting worker. Call at a quiet moment — after the open
 * document is rendered, never while a parse is in flight.
 */
export async function applyUpdate(registration: ServiceWorkerRegistration): Promise<void> {
	const waiting = registration.waiting;
	if (waiting === null) {
		return;
	}
	waiting.postMessage({ type: SKIP_WAITING_MESSAGE });
}
