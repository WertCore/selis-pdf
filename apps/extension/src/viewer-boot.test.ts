/**
 * SL-4.EXT.02 — the redirect target recovers the document URL the rule encoded.
 *
 * The ruleset test in `manifest.test.ts` covers the redirect side; this covers
 * the page that receives it. Together they pin the DoD's preservation clause:
 * the original URL, and the auth context the browser would have sent, both
 * survive the redirect.
 */

import { describe, expect, it } from "vitest";
import { viewerUrlFor } from "./permissions.js";
import {
	ViewerSourceError,
	documentUrlFromViewerLocation,
	fetchInterceptedDocument,
} from "./viewer-boot.js";

/** The `?src=...` tail of a viewer URL, as `location.search` would carry it. */
const searchOf = (doc: string): string => {
	const viewer = viewerUrlFor(doc);
	return viewer.slice(viewer.indexOf("?"));
};

describe("EXT.02 the redirect target recovers the document URL", () => {
	it("round-trips the exact URL the rule encoded", () => {
		const doc = "https://example.com/a.pdf?x=1&y=2#page=3";
		expect(documentUrlFromViewerLocation(searchOf(doc))).toBe(doc);
	});

	it("cannot be tricked into opening a different document", () => {
		const hostile = "https://evil.test/x.pdf?src=https://evil.test/other.pdf";
		expect(documentUrlFromViewerLocation(searchOf(hostile))).toBe(hostile);
	});

	it("reports a typed error when the viewer is opened with no document", () => {
		expect(() => documentUrlFromViewerLocation("")).toThrow(ViewerSourceError);
		try {
			documentUrlFromViewerLocation("");
			throw new Error("expected a throw");
		} catch (e) {
			expect(e).toBeInstanceOf(ViewerSourceError);
			expect((e as ViewerSourceError).reason).toBe("missing-src");
		}
	});

	it("refuses the documented non-http schemes instead of fetching them", () => {
		for (const url of [
			"file:///C:/x.pdf",
			"data:application/pdf;base64,JVBER",
			"blob:https://example.com/550e8400",
		]) {
			try {
				documentUrlFromViewerLocation(searchOf(url));
				throw new Error(`${url} should have been refused`);
			} catch (e) {
				expect(e, url).toBeInstanceOf(ViewerSourceError);
				expect((e as ViewerSourceError).reason, url).toBe("not-fetchable");
			}
		}
	});

	it("fetches with the browser's credentials so auth survives the redirect", () => {
		// The load-bearing DoD clause, asserted on the real fetch options
		// rather than on a comment about them.
		const original = globalThis.fetch;
		const calls: { url: string; init: RequestInit }[] = [];
		globalThis.fetch = ((url: string, init: RequestInit) => {
			calls.push({ url, init: init ?? {} });
			return Promise.resolve(new Response(new Uint8Array([0x25, 0x50])));
		}) as unknown as typeof fetch;
		try {
			void fetchInterceptedDocument("https://example.com/sso/a.pdf");
		} finally {
			globalThis.fetch = original;
		}
		const call = calls.at(0);
		expect(calls, "the viewer must fetch the document exactly once").toHaveLength(1);
		expect(call?.url).toBe("https://example.com/sso/a.pdf");
		expect(call?.init.credentials).toBe("include");
		// A 302 chain resolves here, which is the redirect case DNR cannot see.
		expect(call?.init.redirect).toBe("follow");
	});
});
