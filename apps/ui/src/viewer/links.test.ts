/**
 * Link activation and ADR-P0020 (SL-4.UI.06).
 *
 * Two things are proved here, and the second is the one the plan's DoD is
 * actually about:
 *
 * 1. **The decision is total and pure.** Every action class in the vocabulary
 *    gets exactly one of `internal` / `external` / `blocked`, with a reason on
 *    every refusal, and the same action always gives the same decision.
 * 2. **The disabled paths cannot be reached.** Not "are refused" — *cannot*: a
 *    test drives every ADR-P0020 class through the real controller, against the
 *    real mock adapter, and asserts the mock's `recording.externalUrls` is still
 *    empty afterwards. `/Launch`, `/GoToR`, `/SubmitForm`, `/ImportData` and
 *    document JavaScript have no route to `openExternal`, and this file is where
 *    that is demonstrated rather than asserted in prose.
 *
 * What is **not** proved, and cannot be under ADR-P0021: that a real browser
 * shows a convincing dialog, and that a reader recognises the host in it. The
 * prompt's *content* is asserted — the full destination, verbatim, plus the host
 * — because that is the part this code owns. The dialog's chrome is the shell's
 * and is owed a manual pass, which the viewer README records.
 */

import { describe, expect, it } from "vitest";
import { type MockAdapter, createMockAdapter } from "../platform/mock-adapter.js";
import type { DocHandle, LinkAction, LinkAnnotation, PdfDestination } from "../platform/types.js";
import {
	ALLOWED_EXTERNAL_SCHEMES,
	REFUSAL_FOR_ACTION,
	alignmentForDestination,
	decideLinkAction,
	hostOf,
	isAllowedExternalScheme,
	schemeOf,
} from "./links.js";
import { type PendingExternal, createNavigation } from "./navigation.js";

/** One quad for every fixture link: these tests care about actions, not geometry. */
const BOX = { x: 0, y: 0, width: 1, height: 1 } as const;

function link(action: LinkAction, overrides: Partial<LinkAnnotation> = {}): LinkAnnotation {
	return {
		id: "l1",
		rect: { x: 0, y: 0, width: 10, height: 10 },
		action,
		...overrides,
	};
}

const PAGE_THREE: PdfDestination = { page: 3, kind: "fit" };

describe("scheme parsing (no URL API, per the SL-4.UI.01 globals gate)", () => {
	it("reads a scheme, lowercased", () => {
		expect(schemeOf("https://example.com")).toBe("https:");
		expect(schemeOf("HTTPS://example.com")).toBe("https:");
		expect(schemeOf("mailto:a@b.c")).toBe("mailto:");
		expect(schemeOf("chrome-extension://abc/page.html")).toBe("chrome-extension:");
	});

	it("returns null for a relative reference, which has no scheme", () => {
		expect(schemeOf("chapter3/notes.pdf")).toBeNull();
		expect(schemeOf("/absolute/path.pdf")).toBeNull();
		expect(schemeOf("")).toBeNull();
		// A colon after a path separator belongs to the path, not to a scheme.
		expect(schemeOf("./a:b/c")).toBeNull();
	});

	it("allows http and https and nothing else", () => {
		expect(ALLOWED_EXTERNAL_SCHEMES).toEqual(["http:", "https:"]);
		for (const uri of [
			"https://example.com",
			"http://example.com",
			"HTTPS://EXAMPLE.COM/path?q=1#frag",
		]) {
			expect(isAllowedExternalScheme(uri), uri).toBe(true);
		}
		for (const uri of [
			"javascript:alert(1)",
			"data:text/html;base64,PHNjcmlwdD4=",
			"file:///C:/Windows/System32/calc.exe",
			"mailto:someone@example.com",
			"tel:+441234567890",
			"chrome://settings",
			"ms-msdt:/id",
			"ftp://example.com/file",
			"relative/path.pdf",
		]) {
			expect(isAllowedExternalScheme(uri), uri).toBe(false);
		}
	});
});

describe("host extraction for the prompt", () => {
	it("takes the host and drops the port, path and query", () => {
		expect(hostOf("https://example.com:8443/a/b?c=d")).toBe("example.com");
		expect(hostOf("http://EXAMPLE.com/")).toBe("EXAMPLE.com");
	});

	it("strips userinfo, so a deceptive authority cannot read as a host", () => {
		// The oldest trick in the book: the host is after the `@`.
		expect(hostOf("https://evil.example@good.example/x")).toBe("good.example");
	});

	it("keeps a bracketed IPv6 literal intact", () => {
		expect(hostOf("https://[2001:db8::1]:443/x")).toBe("[2001:db8::1]");
	});

	it("returns null when there is no authority to show", () => {
		expect(hostOf("https:///path")).toBeNull();
		expect(hostOf("https://")).toBeNull();
	});

	describe("the decision table", () => {
		it("navigates inside the document for a resolved /GoTo", () => {
			expect(decideLinkAction({ kind: "goTo", destination: PAGE_THREE })).toEqual({
				kind: "internal",
				destination: PAGE_THREE,
			});
		});

		it("resolves a named destination through the caller's lookup", () => {
			const lookup = (name: string): PdfDestination | undefined =>
				name === "here" ? PAGE_THREE : undefined;
			expect(decideLinkAction({ kind: "goTo", name: "here" }, lookup)).toEqual({
				kind: "internal",
				destination: PAGE_THREE,
			});
			expect(decideLinkAction({ kind: "named", name: "here" }, lookup)).toEqual({
				kind: "internal",
				destination: PAGE_THREE,
			});
		});

		it("prompts for an http(s) URI, with the URL verbatim", () => {
			const url = "https://example.com/redirect?to=https%3A%2F%2Fevil.example&x=1";
			const decision = decideLinkAction({ kind: "uri", uri: url });
			expect(decision).toEqual({ kind: "external", url, host: "example.com" });
			// Verbatim means verbatim: not normalised, not truncated, not re-encoded.
			if (decision.kind === "external") {
				expect(decision.url).toBe(url);
			}
		});

		it("refuses every ADR-P0020 class, each with its own reason", () => {
			// The classes the ADR names, in the ADR's own order.
			expect(decideLinkAction({ kind: "launch", uri: "C:/payload.exe" })).toEqual({
				kind: "blocked",
				reason: "launch",
				action: "launch",
			});
			expect(decideLinkAction({ kind: "goToR" })).toEqual({
				kind: "blocked",
				reason: "remote-destination",
				action: "goToR",
			});
			expect(decideLinkAction({ kind: "submitForm" })).toEqual({
				kind: "blocked",
				reason: "submit-form",
				action: "submitForm",
			});
			expect(decideLinkAction({ kind: "importData" })).toEqual({
				kind: "blocked",
				reason: "import-data",
				action: "importData",
			});
			expect(decideLinkAction({ kind: "javascript" })).toEqual({
				kind: "blocked",
				reason: "javascript",
				action: "javascript",
			});
		});

		it("checks the disabled classes BEFORE any field, so a Launch cannot smuggle a URI", () => {
			// This is the one ordering that matters. A `/Launch` whose `/F` is an
			// http URL is still a launch, and a viewer that checked the string first
			// would open it — which is precisely how `/Launch` comes back to life.
			const decision = decideLinkAction({ kind: "launch", uri: "https://totally-fine.example/" });
			expect(decision.kind).toBe("blocked");
			if (decision.kind === "blocked") {
				expect(decision.reason).toBe("launch");
			}
		});

		it("refuses a scheme it will not open", () => {
			expect(decideLinkAction({ kind: "uri", uri: "javascript:alert(1)" })).toMatchObject({
				kind: "blocked",
				reason: "scheme",
			});
		});

		it("refuses a missing destination rather than guessing a page", () => {
			expect(decideLinkAction({ kind: "goTo", name: "nowhere" }, () => undefined)).toMatchObject({
				kind: "blocked",
				reason: "missing-destination",
			});
			// With no lookup at all, every named destination is missing.
			expect(decideLinkAction({ kind: "named", name: "here" })).toMatchObject({
				kind: "blocked",
				reason: "missing-destination",
			});
		});

		it("refuses an empty or absent target", () => {
			expect(decideLinkAction({ kind: "uri" })).toMatchObject({ reason: "no-target" });
			expect(decideLinkAction({ kind: "uri", uri: "" })).toMatchObject({ reason: "no-target" });
			expect(decideLinkAction({ kind: "none" })).toMatchObject({ reason: "no-target" });
		});

		it("refuses an action class it does not implement", () => {
			expect(decideLinkAction({ kind: "unknown" })).toMatchObject({
				kind: "blocked",
				reason: "unsupported",
			});
		});

		it("covers every action class in the vocabulary, so none is undecided", () => {
			const classes = [
				"goTo",
				"uri",
				"launch",
				"goToR",
				"submitForm",
				"importData",
				"javascript",
				"named",
				"none",
				"unknown",
			] as const;
			for (const kind of classes) {
				const decision = decideLinkAction({ kind });
				expect(["internal", "external", "blocked"], kind).toContain(decision.kind);
				if (decision.kind === "blocked") {
					expect(decision.reason.length, kind).toBeGreaterThan(0);
				}
			}
			// And the refusal table has an entry for exactly the disabled classes.
			expect(Object.keys(REFUSAL_FOR_ACTION).sort()).toEqual([
				"goToR",
				"importData",
				"javascript",
				"launch",
				"submitForm",
			]);
		});

		it("is total: it never throws and never returns undefined", () => {
			for (const kind of ["launch", "uri", "goTo", "named", "none", "unknown"] as const) {
				expect(() => decideLinkAction({ kind })).not.toThrow();
				expect(decideLinkAction({ kind })).toBeDefined();
			}
		});
	});

	describe("destination alignment", () => {
		it("starts a top-of-page destination and centres the rest", () => {
			expect(alignmentForDestination({ page: 0, kind: "fitV" })).toBe("start");
			expect(alignmentForDestination({ page: 0, kind: "fit" })).toBe("centre");
			expect(alignmentForDestination({ page: 0, kind: "xyz", top: 700 })).toBe("centre");
			expect(alignmentForDestination({ page: 0 })).toBe("centre");
		});
	});

	/** A document whose first page carries one link of each interesting class. */
	async function controllerFixture() {
		const adapter = createMockAdapter();
		const descriptor = adapter.addDocument({
			name: "links.pdf",
			pageCount: 8,
			outline: [
				{
					title: "Chapter one",
					destination: { page: 1, kind: "fit" },
					descendantCount: -1,
					children: [{ title: "Figure 1", destination: { page: 2 } }],
				},
			],
			pageLabels: [{ firstPage: 0, style: "r" }],
			destinations: [{ name: "here", destination: { page: 5 } }],
			links: [
				{
					page: 0,
					links: [
						{ rect: BOX, action: { kind: "launch", uri: "C:/payload.exe" } },
						{ rect: BOX, action: { kind: "goToR" } },
						{ rect: BOX, action: { kind: "submitForm" } },
						{ rect: BOX, action: { kind: "importData" } },
						{ rect: BOX, action: { kind: "javascript" } },
						{ rect: BOX, action: { kind: "uri", uri: "javascript:alert(1)" } },
						{ rect: BOX, action: { kind: "uri", uri: "https://example.com/a?b=1" } },
					],
				},
			],
		});
		const doc = await adapter.engine.open(descriptor);
		const navigation = createNavigation({ adapter, doc });
		await navigation.load();
		return { adapter, doc, navigation };
	}

	/**
	 * The document's links, read back **through the port** rather than kept locally.
	 *
	 * Reading them back is the point: it proves the transport delivers the disabled
	 * classes verbatim (see `contract.test.ts`) and that the refusal the controller
	 * applies is the refusal for *the document's own action*, not for a fixture the
	 * test happened to construct.
	 */
	async function linksOf(adapter: MockAdapter, doc: DocHandle): Promise<readonly LinkAnnotation[]> {
		return (await adapter.navigation?.pageLinks(doc, 0)) ?? [];
	}

	describe("ADR-P0020 through the real controller and the real mock host", () => {
		it("cannot reach openExternal through /Launch, /GoToR, /SubmitForm, /ImportData or JS", async () => {
			const { adapter, doc, navigation } = await controllerFixture();
			const links = await linksOf(adapter, doc);
			expect(links).toHaveLength(7);
			for (const candidate of links.slice(0, 6)) {
				const state = navigation.activateLink(candidate);
				expect(state.lastActivation?.decision.kind, candidate.action.kind).toBe("blocked");
				expect(state.pendingExternal, candidate.action.kind).toBeNull();
				// And confirming with nothing pending is a no-op, not a free pass.
				await navigation.confirmExternal();
				expect(adapter.recording.externalUrls, candidate.action.kind).toEqual([]);
			}
		});

		it("prompts with the FULL destination for an http(s) link, and opens nothing yet", async () => {
			const { adapter, doc, navigation } = await controllerFixture();
			const links = await linksOf(adapter, doc);
			const web = links[6];
			expect(web).toBeDefined();
			const state = navigation.activateLink(web as LinkAnnotation);
			const pending = state.pendingExternal;
			expect(pending?.url).toBe("https://example.com/a?b=1");
			expect(pending?.host).toBe("example.com");
			// The full destination is in the prompt body, query string and all.
			expect(state.externalBody(state.pendingExternal as PendingExternal)).toContain(
				"https://example.com/a?b=1",
			);
			// Nothing is opened: the prompt precedes the act, it does not follow it.
			expect(adapter.recording.externalUrls).toEqual([]);
		});

		it("opens exactly once, and only after confirmation", async () => {
			const { adapter, doc, navigation } = await controllerFixture();
			const links = await linksOf(adapter, doc);
			navigation.activateLink(links[6] as LinkAnnotation);
			await navigation.confirmExternal();
			expect(adapter.recording.externalUrls).toEqual(["https://example.com/a?b=1"]);
			// The prompt is consumed: a second confirm does nothing.
			await navigation.confirmExternal();
			expect(adapter.recording.externalUrls).toHaveLength(1);
		});

		it("cancelling the prompt opens nothing", async () => {
			const { adapter, doc, navigation } = await controllerFixture();
			const links = await linksOf(adapter, doc);
			navigation.activateLink(links[6] as LinkAnnotation);
			const state = navigation.cancelExternal();
			expect(state.pendingExternal).toBeNull();
			await navigation.confirmExternal();
			expect(adapter.recording.externalUrls).toEqual([]);
		});

		it("a second link replaces the first prompt rather than queueing it", async () => {
			const { adapter, doc, navigation } = await controllerFixture();
			const links = await linksOf(adapter, doc);
			navigation.activateLink(links[6] as LinkAnnotation);
			const state = navigation.activateLink(links[0] as LinkAnnotation);
			// The second click was a /Launch: refused, and the first prompt is gone
			// because the reader moved on rather than answering it.
			expect(state.pendingExternal).toBeNull();
			await navigation.confirmExternal();
			expect(adapter.recording.externalUrls).toEqual([]);
		});

		it("announces the refusal rather than swallowing the click", async () => {
			const { adapter, doc, navigation } = await controllerFixture();
			const links = await linksOf(adapter, doc);
			const state = navigation.activateLink(links[0] as LinkAnnotation);
			expect(state.refusal).toBe("launch");
			// The sentence names what the document tried to do, not merely "blocked".
			expect(state.announcement).toContain("launch");
			expect(state.announcement.length).toBeGreaterThan(0);
		});

		it("navigates inside the document without touching the host", async () => {
			const { adapter, navigation } = await controllerFixture();
			const internal = link({ kind: "goTo", destination: { page: 4, kind: "xyz", top: 700 } });
			const state = navigation.activateLink(internal);
			expect(state.target).toEqual({ page: 4, align: "centre", from: "link" });
			expect(state.pendingExternal).toBeNull();
			expect(adapter.recording.externalUrls).toEqual([]);
		});

		it("resolves a named /GoTo through the document's own name tree", async () => {
			const { adapter, navigation } = await controllerFixture();
			const named = link({ kind: "named", name: "here" });
			const state = navigation.activateLink(named);
			expect(state.target).toEqual({ page: 5, align: "centre", from: "link" });
			const refused = navigation.activateLink(link({ kind: "named", name: "nowhere" }));
			expect(refused.lastActivation?.decision).toMatchObject({
				kind: "blocked",
				reason: "missing-destination",
			});
			expect(adapter.recording.externalUrls).toEqual([]);
		});
	});
});
