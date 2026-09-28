/**
 * The service worker (SL-4.WEB.02).
 *
 * Registration, scope and lifecycle live here; the caching *policy* is
 * `sw-policy.ts` and is pure, so this file stays a thin adapter whose only jobs
 * are (a) turning platform objects into the policy's plain input types, and
 * (b) performing the two cache writes the policy allows.
 *
 * Lifecycle, and why each step is the way it is:
 *
 * - `install` — precache {@link PRECACHE_PATHS} with `addAll`, which is
 *   atomic: one 404 fails the whole install and the browser keeps the previous
 *   worker. That is the safe failure direction. A half-cached "offline" app is
 *   worse than an online one.
 * - `install` does **not** call `skipWaiting`. The first install still activates
 *   immediately (nothing is waiting), so a first visit is already offline-ready
 *   once the page has loaded. A *replacement* worker waits until the page asks
 *   for it, so a viewer mid-document never has the engine swapped underneath
 *   it; the page sends {@link SKIP_WAITING_MESSAGE} at a quiet moment
 *   (`sw-register.ts`). A viewer that renders a document while its engine
 *   changes is a corruption bug waiting to happen.
 * - `activate` — delete our own stale cache names (never another app's), then
 *   `clients.claim()` so the very first visit is controlled without a reload.
 * - `fetch` — decide, then respond. `decide` is synchronous precisely so the
 *   handler can decide *not* to call `respondWith` at all; a passthrough must
 *   never touch this worker's caches in either direction.
 *
 * Every response this worker returns from a cache is the **cached `Response`
 * object**, never a copy rebuilt from its parts. That is what keeps the WEB.01
 * COOP/COEP headers on the served navigation, and therefore keeps
 * `crossOriginIsolated` true, and therefore keeps the threaded engine path
 * (WASM.03) available offline. The only synthesised response is
 * {@link offlineFallback}, which sets the isolation headers by hand.
 *
 * @see sw-policy.ts for the caching rules and the no-upload argument.
 */

import {
	type Decision,
	PRECACHE_PATHS,
	RUNTIME_CACHE,
	RUNTIME_CACHE_MAX_ENTRIES,
	type RequestFacts,
	type RespondDecision,
	type ResponseFacts,
	SHELL_CACHE,
	SKIP_WAITING_MESSAGE,
	chooseEvictionKey,
	classifyRequest,
	isStaleCacheName,
	mayStoreResponse,
	shellLookupPaths,
} from "./sw-policy.js";

/** What the worker needs from its scope. Injected so the handlers run under Vitest. */
export interface WorkerDeps {
	readonly caches: CacheStorage;
	/** The worker's own `fetch`. Requests it makes are not re-dispatched to itself. */
	readonly fetch: (request: Request) => Promise<Response>;
	/** The worker's scope URL: the origin every cache-eligible request must match. */
	readonly scopeUrl: string;
	readonly clients: { claim(): Promise<void> };
	readonly skipWaiting: () => Promise<void>;
}

/** Read a real `Request` into the policy's plain input type. */
export function requestFactsFrom(request: Request, scopeUrl: string): RequestFacts {
	return {
		url: request.url,
		method: request.method,
		mode: request.mode,
		scopeUrl,
		rangeHeader: request.headers.get("Range"),
		// A `GET` with a body is an upload attempt, not a cache question. A
		// disturbed body still reads as non-null, which is the answer we want.
		hasBody: request.body !== null,
	};
}

/** Read a real `Response` into the policy's plain input type. */
export function responseFactsFrom(response: Response): ResponseFacts {
	const length = response.headers.get("Content-Length");
	const parsed = length === null ? Number.NaN : Number.parseInt(length, 10);
	return {
		ok: response.ok,
		status: response.status,
		type: response.type,
		contentType: response.headers.get("Content-Type") ?? "",
		contentDisposition: response.headers.get("Content-Disposition") ?? "",
		vary: response.headers.get("Vary") ?? "",
		cacheControl: response.headers.get("Cache-Control") ?? "",
		contentLength: Number.isNaN(parsed) ? null : parsed,
	};
}

/**
 * The honest offline response: a 503 that says the app is not installed yet.
 *
 * Synthesised rather than served from a cache, because there is nothing to
 * serve — this is only reached when a shell lookup missed *and* the network
 * failed, which means the user arrived before the first precache completed.
 * The isolation headers are set explicitly so this page is judged by the same
 * rule as the real one, and `no-store` keeps it out of any cache.
 *
 * No inline `<style>`: WEB.01's CSP is `style-src 'self'`, so an inline style
 * here would be blocked and the page would render unstyled. It is plain HTML on
 * purpose.
 */
export function offlineFallback(): Response {
	return new Response(
		'<!doctype html><html lang="en"><head><meta charset="utf-8">' +
			"<title>Selis PDF Viewer — offline</title></head><body>" +
			"<h1>Not available offline yet</h1>" +
			"<p>This copy of the Selis viewer has not finished installing. " +
			"Reconnect once, then it will open local files with no network at all.</p>" +
			"</body></html>",
		{
			status: 503,
			headers: {
				"Content-Type": "text/html; charset=utf-8",
				"Cache-Control": "no-store",
				"Cross-Origin-Opener-Policy": "same-origin",
				"Cross-Origin-Embedder-Policy": "require-corp",
				"Cross-Origin-Resource-Policy": "same-origin",
				"X-Content-Type-Options": "nosniff",
			},
		},
	);
}

/** The event handlers, over injected dependencies. */
export interface WorkerHandlers {
	/** Synchronous on purpose: the `fetch` listener must be able to decline to respond. */
	decide(request: Request): Decision;
	/**
	 * Answer a request the policy accepted. Takes only the `respond: true`
	 * decisions, so a passthrough cannot reach a cache read by type error alone.
	 */
	respond(request: Request, decision: RespondDecision): Promise<Response>;
	install(): Promise<void>;
	activate(): Promise<void>;
}

/**
 * Build the handlers. Pure with respect to the platform: everything it touches
 * arrives through `deps`, so the test suite drives the real lifecycle against
 * an in-memory Cache Storage.
 */
export function createWorkerHandlers(deps: WorkerDeps): WorkerHandlers {
	/**
	 * Cache write #1 of 2. `install` is the only writer of the shell cache, which
	 * is why the fetch handler cannot serve a stale shell: a new version opens a
	 * new cache name rather than mutating this one.
	 */
	async function precacheShell(): Promise<void> {
		const cache = await deps.caches.open(SHELL_CACHE);
		// Atomic by specification. A missing asset fails the install rather than
		// leaving a partially-cached "offline" app.
		await cache.addAll([...PRECACHE_PATHS]);
	}

	/**
	 * Serve the shell. On a miss the network is tried, and the result is returned
	 * but **never stored** — the shell cache is written only by `precacheShell`.
	 */
	async function respondShell(request: Request, path: string): Promise<Response> {
		const cache = await deps.caches.open(SHELL_CACHE);
		for (const candidate of shellLookupPaths(path)) {
			// Keyed by URL, not by Request: the shell has no `Vary` to honour
			// (it is HTML the origin told us not to vary), and the alias list is
			// the whole of the matching logic.
			const hit = await cache.match(candidate);
			if (hit !== undefined) {
				return hit;
			}
		}
		try {
			return await deps.fetch(request);
		} catch {
			return offlineFallback();
		}
	}

	/**
	 * Cache write #2 of 2, and the only one reachable from a live request. Three
	 * independent gates stand between a response and this line: the request
	 * classification, `mayStoreResponse`, and the entry budget below.
	 */
	async function storeRuntimeEntry(
		cache: Cache,
		request: Request,
		response: Response,
	): Promise<void> {
		const keys = await cache.keys();
		const eviction = chooseEvictionKey(
			keys.map((key) => key.url),
			RUNTIME_CACHE_MAX_ENTRIES,
		);
		if (eviction !== null) {
			await cache.delete(eviction);
		}
		// Keyed by the Request so the Cache API honours `Vary` — a brotli-served
		// asset varies on `Accept-Encoding`, and a URL-only key would replay the
		// compressed body to a client that cannot decode it.
		await cache.put(request, response.clone());
	}

	async function respondRuntime(request: Request): Promise<Response> {
		const cache = await deps.caches.open(RUNTIME_CACHE);
		const hit = await cache.match(request);
		if (hit !== undefined) {
			return hit;
		}
		const response = await deps.fetch(request);
		const verdict = mayStoreResponse(responseFactsFrom(response));
		if (verdict.store) {
			try {
				await storeRuntimeEntry(cache, request, response);
			} catch (error) {
				// A failed cache write (quota, private mode) must never turn a
				// working request into an error. The response is still correct;
				// the app simply is not offline-ready yet.
				console.warn("selis: could not cache", request.url, error);
			}
		}
		return response;
	}

	return {
		decide: (request) => classifyRequest(requestFactsFrom(request, deps.scopeUrl)),
		async respond(request, decision) {
			// `decide` never returns a passthrough to this function — the fetch
			// listener checks `respond` first — so the two `respond: true` kinds
			// are exhaustive here and neither is a cache *write* of the shell.
			return decision.kind === "runtime"
				? respondRuntime(request)
				: respondShell(request, decision.path);
		},
		install: precacheShell,
		async activate() {
			for (const name of await deps.caches.keys()) {
				// Ours only. The scope is the whole origin, so a blanket
				// "delete everything" would reach another app's caches.
				if (isStaleCacheName(name)) {
					await deps.caches.delete(name);
				}
			}
			await deps.clients.claim();
		},
	};
}

/** Whether `data` is the page's request to take over. Exported for the tests. */
export function isSkipWaitingMessage(data: unknown): boolean {
	return (
		typeof data === "object" &&
		data !== null &&
		(data as { type?: unknown }).type === SKIP_WAITING_MESSAGE
	);
}

/** The subset of `ServiceWorkerGlobalScope` this module touches. */
interface ServiceWorkerScope {
	addEventListener(type: string, listener: (event: never) => void): void;
	readonly caches: CacheStorage;
	readonly clients: { claim(): Promise<void> };
	readonly registration?: { scope?: string };
	readonly location?: { href?: string };
	skipWaiting(): Promise<void>;
	fetch: typeof fetch;
}

/**
 * The event shapes the wiring needs. Declared here rather than imported from
 * `lib.webworker.d.ts`, which cannot coexist with the DOM lib this package
 * compiles against — and which would say nothing the worker does not honour.
 */
interface ExtendableEventLike {
	waitUntil(promise: Promise<unknown>): void;
}

interface FetchEventLike extends ExtendableEventLike {
	readonly request: Request;
	respondWith(response: Promise<Response>): void;
}

interface MessageEventLike extends ExtendableEventLike {
	readonly data: unknown;
}

/** Whether the current global is a service worker scope, not a window. */
export function isServiceWorkerScope(scope: unknown): boolean {
	const candidate = scope as Partial<ServiceWorkerScope> | null | undefined;
	return (
		typeof candidate === "object" &&
		candidate !== null &&
		typeof candidate.skipWaiting === "function" &&
		typeof candidate.caches === "object" &&
		candidate.caches !== null
	);
}

/**
 * Wire the handlers to the real scope. Guarded rather than top-level so that
 * importing this module in Node (the test suite) has no side effects: a worker
 * that registered listeners on import would be untestable and would fail the
 * `tsc` build's Node-side consumers.
 */
function wire(scope: ServiceWorkerScope): void {
	const handlers = createWorkerHandlers({
		caches: scope.caches,
		// A worker `fetch` never re-dispatches to itself, so this cannot recurse.
		fetch: (request) => scope.fetch(request),
		scopeUrl: scope.registration?.scope ?? scope.location?.href ?? "",
		clients: scope.clients,
		skipWaiting: () => scope.skipWaiting(),
	});

	scope.addEventListener("install", ((event: ExtendableEventLike) => {
		event.waitUntil(handlers.install());
	}) as (event: never) => void);

	scope.addEventListener("activate", ((event: ExtendableEventLike) => {
		event.waitUntil(handlers.activate());
	}) as (event: never) => void);

	scope.addEventListener("fetch", ((event: FetchEventLike) => {
		const decision = handlers.decide(event.request);
		// A passthrough is declined here, before any cache is read: the browser
		// handles the request itself, which is what keeps a cross-origin `?src=`
		// response subject to the origin's own CORS/CORP headers.
		if (decision.respond) {
			event.respondWith(handlers.respond(event.request, decision));
		}
	}) as (event: never) => void);

	scope.addEventListener("message", ((event: MessageEventLike) => {
		if (isSkipWaitingMessage(event.data)) {
			event.waitUntil(scope.skipWaiting());
		}
	}) as (event: never) => void);
}

if (isServiceWorkerScope(globalThis)) {
	wire(globalThis as unknown as ServiceWorkerScope);
}
