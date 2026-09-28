/**
 * The caching policy, tested as policy (SL-4.WEB.02).
 *
 * Two things are being pinned here. The first is the precache/runtime split and
 * its budget, which is a judgement call and should read as one. The second is
 * the document invariant: every entry in the "never cached" table below is a
 * way a real service worker ends up persisting a user's PDF, and each is
 * refused by a rule rather than by care.
 *
 * The table is deliberately exhaustive about the `?src=` shapes (WEB.03) —
 * remote, same-origin, ranged, and un-ranged — because a document that is
 * refused on only one of those grounds is a document one code change away from
 * being cached.
 */

import { describe, expect, it } from "vitest";
import {
	MAX_RUNTIME_ENTRY_BYTES,
	PRECACHE_PATHS,
	RUNTIME_CACHE_MAX_ENTRIES,
	RUNTIME_STORABLE_CONTENT_TYPES,
	SHELL_ALIASES,
	chooseEvictionKey,
	classifyRequest,
	isStaleCacheName,
	mayStoreResponse,
	shellLookupPaths,
} from "./sw-policy.js";
import type { RequestFacts, ResponseFacts } from "./sw-policy.js";

const SCOPE = "https://viewer.selis.test/";

function facts(overrides: Partial<RequestFacts> = {}): RequestFacts {
	return {
		url: `${SCOPE}assets/app.js`,
		method: "GET",
		mode: "cors",
		scopeUrl: SCOPE,
		rangeHeader: null,
		hasBody: false,
		...overrides,
	};
}

function response(overrides: Partial<ResponseFacts> = {}): ResponseFacts {
	return {
		ok: true,
		status: 200,
		type: "basic",
		contentType: "application/wasm",
		contentDisposition: "",
		vary: "",
		cacheControl: "",
		contentLength: 1024,
		...overrides,
	};
}

/** A document response, in every shape an origin might label it. */
function documentResponse(overrides: Partial<ResponseFacts> = {}): ResponseFacts {
	return response({ contentType: "application/pdf", ...overrides });
}

describe("precache set: the boot minimum and nothing else", () => {
	it("holds the shell only - no WASM, no document, no query string", () => {
		for (const path of PRECACHE_PATHS) {
			expect(path.startsWith("/"), path).toBe(true);
			expect(path.includes("?"), path).toBe(false);
			expect(path.endsWith(".wasm"), `${path} must not be precached`).toBe(false);
			expect(path.includes("src="), path).toBe(false);
		}
	});

	it("keeps the WASM engine out of the install, as the size budget requires", () => {
		// The core chunk is up to 3 MB brotli (03-CONVENTIONS.md 12). A first
		// visit must not be a multi-megabyte install, so no module - and no
		// `/wasm/` path at all - appears in the precache list.
		expect(PRECACHE_PATHS).not.toContain("/wasm/selis_pdf_wasm.wasm");
		for (const path of PRECACHE_PATHS) {
			expect(path.startsWith("/wasm/"), path).toBe(false);
			expect(path.endsWith(".wasm"), path).toBe(false);
		}
	});

	it("lists the shell's own assets, so install cannot fail on a missing file", () => {
		expect(PRECACHE_PATHS).toEqual(["/", "/index.html", "/assets/style.css", "/assets/boot.js"]);
	});
});

describe("runtime cache budget", () => {
	it("caps an entry well above the 3 MB brotli core budget", () => {
		// The Cache API stores the decoded body, so the cap has to clear the
		// inflated size of the module the budget protects.
		expect(MAX_RUNTIME_ENTRY_BYTES).toBeGreaterThan(3_000_000);
		expect(RUNTIME_CACHE_MAX_ENTRIES).toBeGreaterThanOrEqual(6); // the six WASM.02 chunks
	});

	it("evicts the oldest key, and only when over the ceiling", () => {
		const keys = ["a", "b", "c"];
		expect(chooseEvictionKey(keys, 3)).toBeNull();
		expect(chooseEvictionKey(keys, 2)).toBe("a");
		expect(chooseEvictionKey([], 0)).toBeNull();
	});
});

describe("requests the worker answers from a cache", () => {
	it("answers a navigation from the shell cache", () => {
		const decision = classifyRequest(facts({ url: `${SCOPE}`, mode: "navigate" }));
		expect(decision).toEqual({ kind: "shell", path: "/", respond: true });
	});

	it("answers the precached assets from the shell cache", () => {
		for (const path of ["/index.html", "/assets/style.css", "/assets/boot.js"]) {
			expect(classifyRequest(facts({ url: `${SCOPE}${path.slice(1)}` }))).toEqual({
				kind: "shell",
				path,
				respond: true,
			});
		}
	});

	it("caches the WASM chunks and the code-split UI at runtime", () => {
		for (const path of [
			"/wasm/selis_pdf_wasm.wasm",
			"/wasm/selis_pdf_wasm_jpx.wasm",
			"/assets/chunk-4f2a.js",
			"/assets/style.css.map",
		]) {
			expect(classifyRequest(facts({ url: `${SCOPE}${path.slice(1)}` }))).toEqual({
				kind: "runtime",
				path,
				respond: true,
			});
		}
	});
});

describe("requests that are never cached - the document table", () => {
	it("refuses a remote ?src= document as cross-origin", () => {
		const decision = classifyRequest(
			facts({
				url: "https://elsewhere.test/report.pdf",
				rangeHeader: "bytes=0-65535",
			}),
		);
		expect(decision.kind).toBe("passthrough");
		expect(decision.respond).toBe(false);
		expect(decision.kind === "passthrough" ? decision.reason : "").toBe("cross-origin");
	});

	it("refuses a same-origin ?src= document on the Range header", () => {
		const decision = classifyRequest(
			facts({ url: `${SCOPE}report.pdf`, rangeHeader: "bytes=0-65535" }),
		);
		expect(decision.kind === "passthrough" ? decision.reason : "").toBe("Range request");
		expect(decision.respond).toBe(false);
	});

	it("refuses a same-origin ?src= document on the query string, and on the Range", () => {
		// Two independent rules, so the refusal does not depend on which shape
		// the caller happens to use: an un-ranged `?src=` open is caught by the
		// query, a ranged one by the `Range` header (and by the query too, if the
		// rule order ever changed).
		const url = `${SCOPE}?src=https%3A%2F%2Fx.test%2Fa.pdf`;
		const ranged = classifyRequest(facts({ url, rangeHeader: "bytes=0-1" }));
		expect(ranged.kind === "passthrough" ? ranged.reason : "").toBe("Range request");
		const plain = classifyRequest(facts({ url }));
		expect(plain.kind === "passthrough" ? plain.reason : "").toBe(
			"URL carries a query string (?src= territory)",
		);
		for (const decision of [ranged, plain]) {
			expect(decision.respond).toBe(false);
		}
	});

	it("refuses a document served from a path that is not on the allow-list", () => {
		for (const url of [
			"report.pdf",
			"uploads/2026/report.pdf",
			"api/document",
			"assets/",
			"wasm/",
		]) {
			const decision = classifyRequest(facts({ url: `${SCOPE}${url}` }));
			expect(decision.kind, url).toBe("passthrough");
			expect(decision.respond, url).toBe(false);
		}
	});

	it("refuses a traversal out of an allow-listed prefix", () => {
		for (const path of [
			"/assets/../report.pdf",
			"/assets/%2e%2e/report.pdf",
			"/assets/%2E%2E%2Freport.pdf",
			"/wasm/../private.pdf",
		]) {
			const decision = classifyRequest(facts({ url: `${SCOPE}${path.slice(1)}` }));
			expect(decision.kind, path).toBe("passthrough");
		}
	});
});

describe("requests the worker refuses on method, body or scheme", () => {
	it("refuses anything that is not a GET", () => {
		for (const method of ["POST", "PUT", "PATCH", "DELETE", "HEAD", "OPTIONS"]) {
			const decision = classifyRequest(facts({ method }));
			expect(decision.kind, method).toBe("passthrough");
			expect(decision.respond, method).toBe(false);
		}
	});

	it("refuses a GET that carries a body - that is an upload attempt", () => {
		const decision = classifyRequest(facts({ hasBody: true }));
		expect(decision.kind === "passthrough" ? decision.reason : "").toContain("upload attempt");
	});

	it("refuses non-http schemes, including the extension origin", () => {
		// The extension reuses this UI in a no-network configuration; if it ever
		// loaded a worker from here, these would be the URLs it would try.
		for (const url of [
			"data:text/plain,hi",
			"blob:https://viewer.selis.test/abc",
			"ftp://viewer.selis.test/report.pdf",
			"chrome-extension://abcdef/assets/app.js",
		]) {
			const decision = classifyRequest(facts({ url }));
			expect(decision.kind, url).toBe("passthrough");
			expect(decision.respond, url).toBe(false);
		}
	});

	it("refuses a passthrough for responding, always", () => {
		// `respond: false` is what keeps the browser (and its own CORS/CORP
		// evaluation) in charge of every request the worker does not own.
		for (const url of ["https://elsewhere.test/a.pdf", "report.pdf", "/assets/a.js?x=1"]) {
			const decision = classifyRequest(facts({ url }));
			expect(decision.respond, url).toBe(false);
		}
	});
});
describe("responses the runtime cache refuses to store", () => {
	it("refuses a document by content type, whatever path served it", () => {
		for (const contentType of [
			"application/pdf",
			"application/pdf; charset=binary",
			"application/x-pdf",
		]) {
			const verdict = mayStoreResponse(documentResponse({ contentType }));
			expect(verdict.store, contentType).toBe(false);
			expect(verdict.store === false ? verdict.reason : "").toContain("document");
		}
	});

	it("refuses octet-stream, which is how a misconfigured origin labels a PDF", () => {
		const verdict = mayStoreResponse(response({ contentType: "application/octet-stream" }));
		expect(verdict.store).toBe(false);
		expect(RUNTIME_STORABLE_CONTENT_TYPES).not.toContain("application/octet-stream");
	});

	it("refuses a document attachment whatever the content type claims", () => {
		const verdict = mayStoreResponse(
			response({ contentType: "text/css", contentDisposition: "attachment; filename=report.pdf" }),
		);
		expect(verdict.store).toBe(false);
	});

	it("refuses a document that lies about its type", () => {
		// Allow-list, not deny-list: `text/plain` is not an asset type we cache,
		// so a PDF served with a wrong Content-Type is still refused.
		for (const contentType of ["text/plain", "text/html", "image/png", ""]) {
			expect(mayStoreResponse(documentResponse({ contentType })).store, contentType).toBe(false);
		}
	});

	it("refuses an opaque body it cannot inspect - the guard fails closed", () => {
		const verdict = mayStoreResponse(documentResponse({ type: "opaque" }));
		expect(verdict.store).toBe(false);
		expect(verdict.store === false ? verdict.reason : "").toContain("cannot be inspected");
	});

	it("refuses a partial or redirected response", () => {
		expect(mayStoreResponse(response({ status: 206, ok: true })).store).toBe(false);
		expect(mayStoreResponse(response({ status: 404, ok: false })).store).toBe(false);
		expect(mayStoreResponse(response({ status: 500, ok: false })).store).toBe(false);
		expect(mayStoreResponse(response({ type: "opaqueredirect" })).store).toBe(false);
	});

	it("refuses what the origin forbade, and what Vary makes uncacheable", () => {
		expect(mayStoreResponse(response({ cacheControl: "no-store" })).store).toBe(false);
		expect(mayStoreResponse(response({ cacheControl: "private, max-age=60" })).store).toBe(false);
		expect(mayStoreResponse(response({ vary: "*" })).store).toBe(false);
	});

	it("refuses an entry over the per-entry cap but still serves it", () => {
		const verdict = mayStoreResponse(response({ contentLength: MAX_RUNTIME_ENTRY_BYTES + 1 }));
		expect(verdict.store).toBe(false);
		expect(verdict.store === false ? verdict.reason : "").toContain("entry cap");
	});

	it("stores the assets it is meant to", () => {
		for (const contentType of [
			"application/wasm",
			"text/css",
			"text/javascript",
			"application/javascript",
			"font/woff2",
			"application/json",
		]) {
			expect(mayStoreResponse(response({ contentType })).store, contentType).toBe(true);
		}
	});

	it("stores an unknown-length asset, since the cap is a ceiling not a floor", () => {
		expect(mayStoreResponse(response({ contentLength: null })).store).toBe(true);
	});
});

describe("shell cache housekeeping", () => {
	it("tries the alias when a host routes / and /index.html differently", () => {
		expect(shellLookupPaths("/")).toEqual(["/", "/index.html"]);
		expect(shellLookupPaths("/index.html")).toEqual(["/index.html", "/"]);
		expect(shellLookupPaths("/assets/style.css")).toEqual(["/assets/style.css"]);
		expect(Object.keys(SHELL_ALIASES).sort()).toEqual(["/", "/index.html"]);
	});

	it("purges our own stale versions and nothing else", () => {
		expect(isStaleCacheName("selis-shell-v0")).toBe(true);
		expect(isStaleCacheName("selis-runtime-v0")).toBe(true);
		expect(isStaleCacheName("selis-shell-v1")).toBe(false);
		expect(isStaleCacheName("workbox-precache-v2")).toBe(false);
		expect(isStaleCacheName("some-other-app-v1")).toBe(false);
	});
});
