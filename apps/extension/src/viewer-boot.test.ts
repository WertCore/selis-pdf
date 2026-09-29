/**
 * SL-4.EXT.02 — the redirect target recovers the document URL the rule encoded.
 *
 * The ruleset test in `manifest.test.ts` covers the redirect side; this covers
 * the page that receives it. Together they pin the DoD's preservation clause:
 * the original URL, and the auth context the browser would have sent, both
 * survive the redirect.
 *
 * SL-4.EXT.09 adds the `file:` case beside it, and the interesting assertions in
 * this file are the ones about what the page **does not do**: it does not refuse
 * a local file, it does not fetch one without the grant, and it does not tell the
 * reader their document is broken when the extension never touched it.
 */

import { describe, expect, it } from "vitest";
import { viewerUrlFor } from "./permissions.js";
import { createFakeHostEnv } from "./ext/fake-host-env.js";
import {
	ViewerSourceError,
	documentUrlFromViewerLocation,
	fetchInterceptedDocument,
	localOutcomeKey,
	localStateKey,
	runLocalFlow,
	sourceErrorKey,
} from "./viewer-boot.js";
import type { ViewerDeps } from "./viewer-boot.js";

/** The `?src=...` tail of a viewer URL, as `location.search` would carry it. */
const searchOf = (doc: string): string => {
	const viewer = viewerUrlFor(doc);
	return viewer.slice(viewer.indexOf("?"));
};

describe("EXT.02 the redirect target recovers the document URL", () => {
	it("round-trips the exact URL the rule encoded", () => {
		const doc = "https://example.com/a.pdf?x=1&y=2#page=3";
		expect(documentUrlFromViewerLocation(searchOf(doc)).url).toBe(doc);
	});

	it("cannot be tricked into opening a different document", () => {
		const hostile = "https://evil.test/x.pdf?src=https://evil.test/other.pdf";
		expect(documentUrlFromViewerLocation(searchOf(hostile)).url).toBe(hostile);
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

	it("classifies http(s) as a web document and nothing else", () => {
		expect(documentUrlFromViewerLocation(searchOf("https://example.com/a.pdf")).kind).toBe("web");
		expect(documentUrlFromViewerLocation(searchOf("http://example.com/a.pdf")).kind).toBe("web");
	});

	it("refuses the non-navigation schemes instead of fetching them", () => {
		// `file:` is deliberately *not* in this list any more: SL-4.EXT.09 gave it
		// a flow, and the assertion below is that the other three still have none.
		for (const url of [
			"data:application/pdf;base64,JVBER",
			"blob:https://example.com/550e8400",
			"wss://example.com/report.pdf",
		]) {
			try {
				documentUrlFromViewerLocation(searchOf(url));
				throw new Error(`${url} should have been refused`);
			} catch (e) {
				expect(e, url).toBeInstanceOf(ViewerSourceError);
				expect((e as ViewerSourceError).reason, url).toBe("unsupported-scheme");
				// The scheme travels with the error so the page can name it instead
				// of saying "unsupported".
				expect((e as ViewerSourceError).scheme, url).toBeTruthy();
			}
		}
	});

	it("refuses an address that is not a URL at all", () => {
		try {
			documentUrlFromViewerLocation(searchOf("not a url"));
			throw new Error("expected a throw");
		} catch (e) {
			expect((e as ViewerSourceError).reason).toBe("invalid-src");
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

	it("drops credentials and the referrer for a local file", () => {
		// The web options are two claims a `file://` request cannot honour: the
		// scheme has no cookies and no origin to leak a referrer to. Passing them
		// through would be noise at best.
		const original = globalThis.fetch;
		const calls: { url: string; init: RequestInit }[] = [];
		globalThis.fetch = ((url: string, init: RequestInit) => {
			calls.push({ url, init: init ?? {} });
			return Promise.resolve(new Response(new Uint8Array([0x25, 0x50])));
		}) as unknown as typeof fetch;
		try {
			void fetchInterceptedDocument("file:///C:/Documents/report.pdf");
		} finally {
			globalThis.fetch = original;
		}
		expect(calls.at(0)?.init.credentials).toBe("omit");
		expect(calls.at(0)?.init.referrerPolicy).toBe("no-referrer");
	});
});

/** A `ViewerDeps` over a fake host env, recording every read it is asked for. */
function harness(overrides: Partial<ViewerDeps> = {}): {
	deps: ViewerDeps;
	env: ReturnType<typeof createFakeHostEnv>;
	reads: string[];
} {
	const fake = createFakeHostEnv();
	const reads: string[] = [];
	return {
		env: fake,
		reads,
		deps: {
			env: fake.env,
			session: () => Promise.reject(new Error("this test must not start an engine")),
			read: (url) => {
				reads.push(url);
				return Promise.reject(new Error("this test must not read a document"));
			},
			...overrides,
		},
	};
}

const LOCAL = "file:///C:/Users/x/Documents/report.pdf";

describe("EXT.09 the local-file flow never touches a file it is not entitled to", () => {
	it("does not read the file at all when the toggle is off", async () => {
		// The single most important assertion in this task. Before the grant DNR
		// does not intercept and a `file://` read fails naming no cause, so the
		// only way to avoid telling the reader their document is broken is to not
		// attempt it and say what is actually known.
		const { deps, reads, env } = harness();
		env.recording.fileAccess = "withheld";
		const state = await runLocalFlow(deps, LOCAL);
		expect(reads, "a withheld grant must not produce a read").toEqual([]);
		expect(state).toEqual({ access: "withheld", outcome: "request" });
	});

	it("reads the file when the toggle is on, and opens it", async () => {
		const { deps, reads, env } = harness({
			read: (url) => {
				reads.push(url);
				return Promise.resolve(new ArrayBuffer(8));
			},
		});
		env.recording.fileAccess = "granted";
		const state = await runLocalFlow(deps, LOCAL);
		expect(reads).toEqual([LOCAL]);
		expect(state).toEqual({ access: "granted", outcome: "open" });
	});

	it("blames the file, not the permission, when a granted read fails", async () => {
		// Two different causes and two different sentences: sending a reader whose
		// switch is already on back to the switch is the failure this prevents.
		const { deps, env } = harness();
		env.recording.fileAccess = "granted";
		expect((await runLocalFlow(deps, LOCAL)).outcome).toBe("unreadable");
	});

	it("treats a browser that cannot answer as unknown, never as a refusal", async () => {
		const { deps, reads, env } = harness();
		env.failFileAccess(new Error("chrome.extension is not a function"));
		const state = await runLocalFlow(deps, LOCAL);
		expect(reads).toEqual([]);
		expect(state).toEqual({ access: "unknown", outcome: "unchecked" });
	});

	it("asks the browser exactly once per run", async () => {
		const { deps, env } = harness();
		await runLocalFlow(deps, LOCAL);
		expect(env.recording.fileAccessCalls).toBe(1);
	});

	it("reads nothing for the toolbar popup, which has no file in hand", async () => {
		// The popup is the entry point for "open a local PDF", and it must not
		// start an engine on the reader's behalf to say so.
		const { deps, reads, env } = harness();
		const state = await runLocalFlow(deps, "");
		expect(reads).toEqual([]);
		expect(env.recording.fileAccessCalls).toBe(1);
		expect(state.outcome).toBe("request");
	});
});

describe("EXT.09 every state has its own words", () => {
	it("gives each access state a distinct status line", () => {
		const keys = (["granted", "withheld", "unknown"] as const).map(localStateKey);
		expect(new Set(keys).size).toBe(3);
	});

	it("gives each non-opening outcome its own sentence, and none for success", () => {
		expect(localOutcomeKey("open")).toBeNull();
		expect(localOutcomeKey("request")).toBe("viewer.local.denied");
		expect(localOutcomeKey("unreadable")).toBe("viewer.local.unreadable");
		expect(localOutcomeKey("unchecked")).toBe("viewer.local.unchecked");
	});

	it("says nothing about a source the viewer was simply not given", () => {
		// `missing-src` is the toolbar button, not a failure; a line of red text
		// there is the old "use the toolbar button" bug in a new coat.
		expect(sourceErrorKey("missing-src")).toBeNull();
		expect(sourceErrorKey("invalid-src")).toBe("viewer.status.invalidSource");
		expect(sourceErrorKey("unsupported-scheme")).toBe("viewer.status.unsupportedScheme");
	});
});
