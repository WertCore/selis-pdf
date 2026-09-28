/**
 * The service worker's caching policy (SL-4.WEB.02).
 *
 * "Cache the app shell and WASM chunks; the app opens local files with no
 * network at all." This module is that sentence as data and pure functions: no
 * DOM, no `caches`, no `fetch`, so the whole policy runs under Vitest in Node.
 * `sw.ts` is the thin adapter that hands real `Request`/`Response` objects to
 * these functions and performs the two cache writes the policy permits.
 *
 * ## The one invariant everything else serves
 *
 * **A document is never cached.** A service worker is the single most likely
 * place to accidentally persist one — a runtime cache keyed on a URL, an
 * opaque response replayed to a later `?src=` open, a `206` stored and later
 * served whole. So the rule here is not "don't cache documents", it is
 * "**only these URLs, and only these content types, can ever be written**":
 *
 * 1. A request is cache-eligible only if it is a bodyless `GET`, has no
 *    `Range` header, carries **no query string**, is **same-origin with the
 *    worker's scope**, and its path is on a fixed allow-list. Everything else
 *    is a passthrough: the worker does not call `respondWith`, does not read a
 *    cache, and does not write one. The default is *no*.
 * 2. A response is storable only if its content type is on a fixed allow-list
 *    (`application/pdf` and `application/octet-stream` are not on it), it is a
 *    complete 200, and the origin did not say `no-store`. The check runs on the
 *    response too, so a document served *from an allow-listed path* is still
 *    refused.
 * 3. There are exactly two places in this package that write to a cache:
 *    `install` (the shell, from the fixed {@link PRECACHE_PATHS}) and the
 *    runtime branch of the fetch handler (allow-listed paths, storable
 *    responses only). A test greps the built worker for cache-write calls and
 *    fails if a third call site appears.
 *
 * The `Range` and query-string rules are what make the `?src=` path
 * structurally uncacheable: WEB.03 pulls a remote document in with
 * `GET <url>` + `Range` + no body (`handoff-flow.ts`), so every one of those
 * requests is refused by rule 1 on three independent grounds before the URL is
 * ever looked at, and the response is refused again by rule 2.
 *
 * ## Why a cache cannot weaken the no-upload gate (ADR-P0016)
 *
 * WEB.03 wraps the page's `fetch`/`XMLHttpRequest.prototype.send`. The page's
 * wrapper runs **before** the browser dispatches the request, and the service
 * worker's `fetch` event fires **after** that dispatch — the worker is
 * downstream of the gate, not upstream of it. A cached response changes what
 * comes *back*; it cannot change what the page already recorded going out. The
 * two halves therefore compose: the gate keeps proving nothing is uploaded,
 * and this module keeps proving nothing is persisted. Both directions are
 * tested (`sw.test.ts`, "the SW does not bypass the WEB.03 gate").
 *
 * ## COOP/COEP (WEB.01, inherited)
 *
 * Cross-origin isolation is decided by the response of the top-level
 * *navigation*, so serving the shell from a cache is exactly the case where
 * this worker could quietly break `crossOriginIsolated` and demote the threaded
 * engine to single-threaded (WASM.03, ADR-P0004). Two rules keep it intact:
 * the worker returns the **cached `Response` object verbatim** — it never
 * rebuilds one from parts, which is how COOP/COEP would get stripped — and the
 * only response it ever synthesises is the offline 503, which carries the
 * isolation headers explicitly (see `offlineFallback` in `sw.ts`).
 */

/**
 * Cache namespace version. Bump on every release that changes the shell or the
 * asset set: a new worker opens `…-v<N+1>` and `activate` deletes the old
 * name, so a stale shell is never served across a version boundary. The
 * browser's own update detection (a changed `sw.js`, byte for byte) is the
 * trigger; this constant is the *namespace*, not the trigger.
 */
export const CACHE_VERSION = "1";

/** Prefix every cache this app owns carries, so `activate` purges our own and nothing else. */
export const CACHE_PREFIX = "selis-";

/** The app shell: precached at install, read-only for the lifetime of a version. */
export const SHELL_CACHE = `${CACHE_PREFIX}shell-v${CACHE_VERSION}`;

/** Same-origin static assets (JS/CSS chunks, WASM modules, fonts) cached on first use. */
export const RUNTIME_CACHE = `${CACHE_PREFIX}runtime-v${CACHE_VERSION}`;

/** The only cache names this version owns. Everything else under our prefix is purged on activate. */
export const OWNED_CACHES: readonly string[] = [SHELL_CACHE, RUNTIME_CACHE];

/** Where the worker is served from, and the scope it controls. */
export const SERVICE_WORKER_PATH = "/sw.js";
export const SW_SCOPE = "/";

/**
 * The precache list: the minimum needed to boot with no network at all.
 *
 * Small on purpose. The core WASM module alone is up to 3 MB brotli
 * (`03-CONVENTIONS.md §12`, `xtask/size-budgets.toml`); putting it here would
 * make a first visit a multi-megabyte install for every visitor, including the
 * many who will never open a PDF. So the shell is HTML + CSS + the boot
 * script, and the engine is fetched once, on demand, into the runtime cache
 * (see {@link RUNTIME_PATH_PREFIXES}). Same reasoning as EXT.05, which keeps
 * CJK fonts an optional download rather than a bundled asset — the extension's
 * budget is tighter, the principle is identical.
 *
 * `"/"` and `"/index.html"` are the same document; both are listed so a
 * navigation to either resolves from cache regardless of how the static host
 * routes them (see {@link SHELL_ALIASES}).
 */
export const PRECACHE_PATHS: readonly string[] = [
	"/",
	"/index.html",
	"/assets/style.css",
	"/assets/boot.js",
];

/**
 * One shell path stands in for another when the host routes `/` and
 * `/index.html` differently. Without this, precaching `/` and then navigating
 * to `/index.html` offline would miss and 503.
 */
export const SHELL_ALIASES: Readonly<Record<string, readonly string[]>> = {
	"/": ["/index.html"],
	"/index.html": ["/"],
};

/**
 * Path prefixes eligible for the runtime cache.
 *
 * `/assets/` is the built UI (code-split chunks from UI.02, content-hashed by
 * any real bundler) and `/wasm/` is where the WASM.02 chunk manifest points
 * (`packages/wasm-loader/src/manifest.ts` resolves `wasm/selis_pdf_wasm.wasm`
 * relative to the shell). The viewer loads `core` eagerly and `jpx`/`cjk` on
 * demand, so runtime caching is what makes a *second* session work with no
 * network without a 3 MB upfront install. `ocr`/`convert`/`editor` are never
 * fetched by a viewer session (WASM.02 enforces it); if one were, caching it
 * would be correct anyway — the question is whether the user asked for it.
 */
export const RUNTIME_PATH_PREFIXES: readonly string[] = ["/assets/", "/wasm/"];

/**
 * Content types a response must have to be stored. An allow-list, not a
 * deny-list: `application/pdf` and `application/octet-stream` are absent, so a
 * document is refused whatever path it was served from and whatever the origin
 * labelled it.
 */
export const RUNTIME_STORABLE_CONTENT_TYPES: readonly string[] = [
	"text/css",
	"text/javascript",
	"application/javascript",
	"application/json",
	"application/wasm",
	"font/woff2",
	"font/ttf",
	"image/svg+xml",
];

/**
 * Largest single entry the runtime cache will store, in bytes.
 *
 * 12 MB is 4x the 3 MB brotli core-WASM budget: the Cache API stores the
 * *decoded* body, so the stored figure for a brotli-served module is its
 * inflated size, and a cap set at the brotli budget would reject the very
 * module the budget exists to protect. An entry over this cap is still served —
 * it is simply not remembered.
 */
export const MAX_RUNTIME_ENTRY_BYTES = 12_000_000;

/**
 * Entry ceiling for the runtime cache. WASM.02 ships six chunks; with UI
 * code-splitting this is generous. It exists so a long-lived install cannot
 * grow without bound, and so the eviction order is testable.
 */
export const RUNTIME_CACHE_MAX_ENTRIES = 32;

/** Message a page posts to make a waiting worker take over (see `sw-register.ts`). */
export const SKIP_WAITING_MESSAGE = "selis:skip-waiting";

/** The header names the served navigation must keep to stay `crossOriginIsolated`. */
export const ISOLATION_HEADER_NAMES = [
	"Cross-Origin-Opener-Policy",
	"Cross-Origin-Embedder-Policy",
] as const;

/** The pure description of a request the policy reasons about. */
export interface RequestFacts {
	readonly url: string;
	readonly method: string;
	/** `Request.mode`; `"navigate"` is the only value answered from the shell cache. */
	readonly mode: string;
	/** The worker's own scope URL — the origin every cacheable request must match. */
	readonly scopeUrl: string;
	/** Value of the `Range` request header, or `null` when absent. */
	readonly rangeHeader: string | null;
	/** Whether the request carries a body. A `GET` with a body is an upload attempt. */
	readonly hasBody: boolean;
}

/** What the worker should do with one request. */
export type Decision =
	| {
			readonly kind: "shell";
			readonly path: string;
			/** Look in the shell cache, then the network. Never written outside `install`. */
			readonly respond: true;
	  }
	| {
			readonly kind: "runtime";
			readonly path: string;
			/** Cache-first, then the network, storing the response if it is storable. */
			readonly respond: true;
	  }
	| {
			readonly kind: "passthrough";
			/** Why this request is ineligible — asserted by the tests, so it stays specific. */
			readonly reason: string;
			/**
			 * Always `false`: the worker must not call `respondWith` at all.
			 *
			 * Not an optimisation. A cross-origin or opaque response this worker
			 * returned would be re-evaluated for COEP/CORP against the response
			 * *we* produced, so letting the browser handle the request itself is
			 * what preserves the WEB.01 isolation contract for `?src=` origins.
			 */
			readonly respond: false;
	  };

/** The decisions the fetch listener is allowed to answer. */
export type RespondDecision = Extract<Decision, { readonly respond: true }>;

function parse(url: string, scopeUrl: string): URL | null {
	try {
		return new URL(url, scopeUrl);
	} catch {
		return null;
	}
}

function sameOrigin(url: string, scopeUrl: string): boolean {
	const parsed = parse(url, scopeUrl);
	const scope = parse(scopeUrl, scopeUrl);
	return parsed !== null && scope !== null && parsed.origin === scope.origin;
}

function pathOf(url: string, scopeUrl: string): string {
	return parse(url, scopeUrl)?.pathname ?? "";
}

function hasQuery(url: string, scopeUrl: string): boolean {
	const parsed = parse(url, scopeUrl);
	// An unparseable URL is not eligible for anything; report it as query-bearing
	// so it lands in the passthrough branch.
	return parsed === null || parsed.search !== "";
}

function isHttpUrl(url: string, scopeUrl: string): boolean {
	const protocol = parse(url, scopeUrl)?.protocol;
	return protocol === "https:" || protocol === "http:";
}

/** Whether `path` sits under a runtime prefix, with a real file name after it. */
function isRuntimePath(path: string): boolean {
	// `..` and its percent-encoded spellings are refused outright. `URL`
	// normalisation already folds a literal `..`, so this is the belt to that
	// braces: an encoded traversal must not resolve to an allow-listed prefix
	// either.
	const lower = path.toLowerCase();
	if (lower.includes("..") || lower.includes("%2e")) {
		return false;
	}
	return RUNTIME_PATH_PREFIXES.some(
		(prefix) => path.startsWith(prefix) && path.length > prefix.length,
	);
}
/**
 * Decide how to handle one request. Most specific rule first; every rule
 * carries the reason it exists, and the tests assert those reasons.
 *
 * The order matters in one place only: `Range` and the query string are checked
 * *before* the path allow-list, so a `?src=` document is refused on those
 * grounds even when it is served from `/assets/`. The path allow-list is the
 * last gate, not the first.
 */
export function classifyRequest(facts: RequestFacts): Decision {
	const method = facts.method.toUpperCase();
	if (method !== "GET") {
		// `HEAD` is excluded deliberately, not by habit: Cache Storage matches on
		// URL and `Vary`, not on method, so a stored bodyless `HEAD` response
		// would be handed to a later `GET` as the whole asset.
		return {
			kind: "passthrough",
			reason: `method ${method}: only GET is cacheable`,
			respond: false,
		};
	}
	if (facts.hasBody) {
		return {
			kind: "passthrough",
			reason: "request carries a body; a GET with a body is an upload attempt (ADR-P0016)",
			respond: false,
		};
	}
	if (!isHttpUrl(facts.url, facts.scopeUrl)) {
		return { kind: "passthrough", reason: "not an http(s) URL", respond: false };
	}
	if (!sameOrigin(facts.url, facts.scopeUrl)) {
		// A remote `?src=` document, and every other cross-origin request.
		return { kind: "passthrough", reason: "cross-origin", respond: false };
	}
	if (facts.rangeHeader !== null) {
		// The `?src=` download path (WEB.03) and any media seek. A 206 must never
		// be stored (it is a fragment) and a stored full body must never be
		// replayed against a `Range` (the caller would read the wrong window).
		return { kind: "passthrough", reason: "Range request", respond: false };
	}
	if (hasQuery(facts.url, facts.scopeUrl)) {
		// `?src=` is a query parameter. The viewer uses no other query string, so
		// this is a cheap structural block on the document-opening URL itself.
		return {
			kind: "passthrough",
			reason: "URL carries a query string (?src= territory)",
			respond: false,
		};
	}

	const path = pathOf(facts.url, facts.scopeUrl);
	if (facts.mode === "navigate" || PRECACHE_PATHS.includes(path)) {
		return { kind: "shell", path, respond: true };
	}
	if (isRuntimePath(path)) {
		return { kind: "runtime", path, respond: true };
	}
	// Default deny. A path nobody allow-listed is not cached in either direction:
	// it is neither stored nor served from a cache.
	return {
		kind: "passthrough",
		reason: `path ${path} is not on the cache allow-list`,
		respond: false,
	};
}

/** The pure description of a response the policy reasons about. */
export interface ResponseFacts {
	readonly ok: boolean;
	readonly status: number;
	/** `Response.type`; `"opaque"` means a no-cors response with an unreadable body. */
	readonly type: string;
	readonly contentType: string;
	readonly contentDisposition: string;
	readonly vary: string;
	readonly cacheControl: string;
	/** `Content-Length`, or `null` when the origin did not say. */
	readonly contentLength: number | null;
}

export type StoreDecision =
	| { readonly store: true }
	| { readonly store: false; readonly reason: string };

/**
 * Whether one response may be written to the runtime cache.
 *
 * The content-type allow-list is the load-bearing check: it runs on the
 * *response*, so a document is refused even if it arrived on a path the
 * allow-list would otherwise accept. `application/octet-stream` is deliberately
 * absent — it is what a misconfigured origin most likely labels a PDF with, and
 * the WASM modules are served as `application/wasm`.
 */
export function mayStoreResponse(
	facts: ResponseFacts,
	maxEntryBytes = MAX_RUNTIME_ENTRY_BYTES,
): StoreDecision {
	if (facts.type === "opaque" || facts.type === "opaqueredirect") {
		// An opaque body cannot be inspected, and this guard fails closed
		// everywhere else too (`no-upload.ts`).
		return { store: false, reason: "opaque response: its body cannot be inspected" };
	}
	if (!facts.ok || facts.status !== 200) {
		return { store: false, reason: `status ${facts.status} is not a complete 200` };
	}
	if (facts.vary === "*") {
		return { store: false, reason: "Vary: * is uncacheable by specification" };
	}
	const cacheControl = facts.cacheControl.toLowerCase();
	if (cacheControl.includes("no-store") || cacheControl.includes("private")) {
		return { store: false, reason: `origin said Cache-Control: ${facts.cacheControl}` };
	}
	if (facts.contentDisposition.toLowerCase().includes("attachment")) {
		return { store: false, reason: "Content-Disposition: attachment (a document)" };
	}
	const contentType = facts.contentType.split(";")[0]?.trim().toLowerCase() ?? "";
	if (contentType === "application/pdf" || contentType === "application/x-pdf") {
		return { store: false, reason: "Content-Type: application/pdf (a document)" };
	}
	if (!RUNTIME_STORABLE_CONTENT_TYPES.includes(contentType)) {
		return {
			store: false,
			reason: `Content-Type ${contentType || "(absent)"} is not an allow-listed asset type`,
		};
	}
	if (facts.contentLength !== null && facts.contentLength > maxEntryBytes) {
		return {
			store: false,
			reason: `${facts.contentLength} bytes exceeds the ${maxEntryBytes} byte entry cap`,
		};
	}
	return { store: true };
}

/** Shell paths to try, in order, when looking one up in the shell cache. */
export function shellLookupPaths(path: string): string[] {
	const aliases = SHELL_ALIASES[path] ?? [];
	return [path, ...aliases];
}

/** Caches `activate` should delete: ours, from a previous version, and nothing else. */
export function isStaleCacheName(name: string): boolean {
	return name.startsWith(CACHE_PREFIX) && !OWNED_CACHES.includes(name);
}

/**
 * Which cached key to evict when the runtime cache is over its entry ceiling.
 * Cache Storage returns `keys()` in insertion order, so the first key is the
 * least recently added. Returns the key to delete, or `null` when the cache is
 * within budget.
 */
export function chooseEvictionKey(
	keys: readonly string[],
	maxEntries = RUNTIME_CACHE_MAX_ENTRIES,
): string | null {
	if (keys.length <= maxEntries) {
		return null;
	}
	return keys[0] ?? null;
}
