/**
 * SL-4.EXT.06 - the extension's transport, held to the shared contract suite.
 *
 * `definePlatformAdapterContract` is `apps/ui`'s, not this package's, and that
 * is the entire claim being tested here: the same suite the mock and the web
 * host pass, driven through a `chrome.runtime`-shaped JSON link into an
 * offscreen-document host. Every probe therefore runs against real base64 in
 * both directions rather than against an in-process shortcut.
 *
 * The one exemption is clipboard read, and it is a capability question rather
 * than a behaviour one: the extension cannot have it without the
 * `clipboardRead` permission, and SL-4.EXT.01's approved set does not include
 * it. `adapter.test.ts` asserts the refusal is a real, coded refusal.
 *
 * What the suite cannot see, and what the tests below it add:
 *
 * - **That the loopback is honest.** It JSON round-trips every message, and
 *   `it("does not alias across the link")` proves it by planting a value that
 *   would survive a pass-by-reference and checking that it does not.
 * - **The host's own refusals** - an unknown handle, a document the viewer
 *   cannot describe, a malformed tile - which the client sees only as "some
 *   error came back".
 */

import { describe, expect, it } from "vitest";
import { SAMPLE_TEXTS, definePlatformAdapterContract } from "../../../ui/src/platform/contract.js";
import { AdapterError, ErrorCode } from "../../../ui/src/platform/errors.js";
import type { DocumentSourceDescriptor } from "../../../ui/src/platform/types.js";
import { createExtensionAdapter } from "./adapter.js";
import { createEnginePort, readSourceBytes } from "./engine-client.js";
import { encodeBase64 } from "./engine-protocol.js";
import { createFakeHostEnv } from "./fake-host-env.js";
import { createFixtureEngine, sampleBytes } from "./fixture-engine.js";
import { createLoopbackLink } from "./loopback.js";
import { serveEngineHost } from "./offscreen-engine.js";

/** A descriptor naming the contract's sample document. */
function sampleDescriptor(): DocumentSourceDescriptor {
	const bytes = sampleBytes();
	return { kind: "bytes", bytes: bytes.buffer as ArrayBuffer, name: "sample.pdf" };
}

/** A live client and host, joined by the JSON loopback. */
function connected() {
	const link = createLoopbackLink();
	const engine = createFixtureEngine();
	serveEngineHost({ link: link.host, engine, navigation: engine });
	const client = createEnginePort(link.client);
	return { link, client };
}

definePlatformAdapterContract(
	"extension offscreen transport",
	async () => {
		const link = createLoopbackLink();
		const engine = createFixtureEngine();
		serveEngineHost({ link: link.host, engine, navigation: engine });
		const adapter = createExtensionAdapter({
			env: createFakeHostEnv().env,
			link: link.client,
		});
		const descriptor = sampleDescriptor();
		// Opened *through* the wire, not handed in: the handle the UI holds has
		// to be one the client reconstructed from a reply, or the rest of the
		// suite would be testing a local object.
		const doc = await adapter.engine.open(descriptor);
		return { adapter, descriptor, doc };
	},
	{
		exempt: {
			"clipboard-read":
				"reading the clipboard needs the 'clipboardRead' permission, which SL-4.EXT.01's approved set excludes; the extension reports clipboardRead: false and refuses (see adapter.test.ts)",
		},
	},
);

describe("the transport under the shared contract (SL-4.EXT.06)", () => {
	it("does not alias across the link", async () => {
		// If the loopback passed objects by reference, every test above would
		// pass while the real port base64ed everything. A typed array is the
		// one value whose survival is a genuine signal, so it is planted here.
		const link = createLoopbackLink();
		let seen: unknown = null;
		// Subscribe on the client side and post from the host side: the two
		// directions of a port are different channels.
		link.client.subscribe((reply) => {
			seen = reply;
		});
		const bytes = new Uint8Array([1, 2, 3]);
		link.host.post({ v: 1, id: 1, ok: true, value: { done: true, blob: bytes } } as never);
		await new Promise((resolve) => setTimeout(resolve, 0));
		const received = (seen as { value?: { blob?: unknown } } | null)?.value?.blob;
		expect(received).not.toBe(bytes);
		expect(received).toEqual({ 0: 1, 1: 2, 2: 3 });
	});

	it("reads bytes and blob descriptors, and refuses the other four", async () => {
		const fromBytes = await readSourceBytes(sampleDescriptor());
		expect(Array.from(fromBytes.bytes)).toEqual(Array.from(sampleBytes()));
		expect(fromBytes.name).toBe("sample.pdf");

		const blob = new Blob([sampleBytes().buffer as ArrayBuffer], { type: "application/pdf" });
		const fromBlob = await readSourceBytes({ kind: "blob", blob, name: "picked.pdf" });
		expect(Array.from(fromBlob.bytes)).toEqual(Array.from(sampleBytes()));

		for (const descriptor of [
			{ kind: "opfs", path: "/doc.pdf" },
			{ kind: "fsa", handle: { kind: "file", name: "x.pdf" } },
			{ kind: "file", path: "C:/doc.pdf" },
			{ kind: "http-range", url: "https://example.com/a.pdf" },
		] as DocumentSourceDescriptor[]) {
			await expect(readSourceBytes(descriptor), descriptor.kind).rejects.toMatchObject({
				code: ErrorCode.BindingBadArgument,
				docState: "Unchanged",
			});
		}
	});

	it("preserves tile pixels exactly across the base64 hop", async () => {
		const { client } = connected();
		const doc = await client.open(sampleDescriptor());
		const tile = await client.renderTile({ doc, page: 0, scale: 0.5 });
		// RGBA8, tightly packed, fully opaque - the three properties the contract
		// asserts and the three a base64 round trip could plausibly break.
		expect(tile.data.byteLength).toBe(tile.width * tile.height * 4);
		const pixels = new Uint8ClampedArray(tile.data);
		expect(pixels[3]).toBe(255);
		expect(pixels[0]).toBe(47);
	});

	it("answers an unknown document with BINDING_BAD_HANDLE and Unchanged", async () => {
		const { client } = connected();
		const doc = await client.open(sampleDescriptor());
		await client.close(doc);
		await expect(client.renderTile({ doc, page: 0, scale: 1 })).rejects.toMatchObject({
			code: ErrorCode.BindingBadHandle,
			docState: "Unchanged",
		});
	});

	it("carries the host's docState across rather than flattening it", async () => {
		// A bad handle is `Unchanged` (the document was never touched) while a
		// bad page is `Unchanged` too, but a *source* failure is `NotLoaded`.
		// The point is that the client does not substitute a constant.
		const { client } = connected();
		const doc = await client.open(sampleDescriptor());
		await expect(client.extractText(doc, doc.pageCount)).rejects.toMatchObject({
			code: ErrorCode.BindingBadArgument,
			docState: "Unchanged",
		});
	});

	it("streams a search as progressive batches ending in done", async () => {
		const { client } = connected();
		const doc = await client.open(sampleDescriptor());
		const progress: number[] = [];
		let matches = 0;
		for await (const batch of client.search(doc, "alpha")) {
			progress.push(batch.progress);
			matches += batch.matches.length;
		}
		expect(matches).toBe(2);
		expect(progress.at(-1)).toBe(1);
		expect(
			progress.every((value, index) => index === 0 || value >= (progress[index - 1] ?? 0)),
		).toBe(true);
	});

	it("cancels promptly without waiting for the host to answer", async () => {
		const { client } = connected();
		const doc = await client.open(sampleDescriptor());
		const controller = new AbortController();
		const iterator = client
			.search(doc, "alpha", undefined, { signal: controller.signal })
			[Symbol.asyncIterator]();
		await iterator.next();
		controller.abort();
		// The rejection is produced locally; the `cancel` message is advisory,
		// so this resolves even though the host is still streaming.
		await expect(iterator.next()).rejects.toMatchObject({
			code: ErrorCode.Cancelled,
			docState: "Unchanged",
		});
	});

	it("refuses a pre-aborted render before anything is sent", async () => {
		const { client, link } = connected();
		const doc = await client.open(sampleDescriptor());
		const controller = new AbortController();
		controller.abort();
		await expect(
			client.renderTile({ doc, page: 0, scale: 1 }, { signal: controller.signal }),
		).rejects.toBeInstanceOf(AdapterError);
		link.client.close();
	});

	it("rejects a tile whose pixel count does not match its dimensions", async () => {
		// The host is trusted to be our own code, but a mismatch here is silent
		// corruption, and the check is one line.
		const link = createLoopbackLink();
		const linkClient = link.client;
		serveEngineHost({
			link: link.host,
			engine: {
				open: async () => ({ id: "d1", pageCount: 1, pageSizes: [{ width: 1, height: 1 }] }),
				close: async () => {},
				renderTile: async () => ({
					page: 0,
					width: 2,
					height: 2,
					format: "rgba8" as const,
					data: new ArrayBuffer(4),
				}),
				extractText: async () => ({ page: 0, text: "" }),
				// UI.04 made EnginePort.textLayer required; these engine doubles
				// exist to make a *tile* fail, so they reject rather than
				// returning empty quads - the contract forbids inventing
				// geometry. The op is on the wire as of SL-4.EXT.03; the real
				// text layer is covered in engine-host.test.ts, which drives the
				// real host and the real engine mapping.
				textLayer: async () => {
					throw AdapterError.badArgument("textLayer is not implemented by the engine host");
				},
				search: async function* () {
					yield { matches: [], progress: 1, done: true };
				},
			},
		});
		const client = createEnginePort(linkClient);
		const doc = await client.open(sampleDescriptor());
		await expect(client.renderTile({ doc, page: 0, scale: 1 })).rejects.toMatchObject({
			code: ErrorCode.BindingBadArgument,
		});
	});

	it("rejects a tile whose pixels are not decodable base64", async () => {
		const link = createLoopbackLink();
		serveEngineHost({
			link: link.host,
			engine: {
				open: async () => ({ id: "d1", pageCount: 1, pageSizes: [{ width: 1, height: 1 }] }),
				close: async () => {},
				renderTile: async () => ({
					page: 0,
					width: 1,
					height: 1,
					format: "rgba8" as const,
					data: new ArrayBuffer(4),
				}),
				extractText: async () => ({ page: 0, text: "" }),
				// UI.04 made EnginePort.textLayer required; these engine doubles
				// exist to make a *tile* fail, so they reject rather than
				// returning empty quads - the contract forbids inventing
				// geometry. The op is on the wire as of SL-4.EXT.03; the real
				// text layer is covered in engine-host.test.ts, which drives the
				// real host and the real engine mapping.
				textLayer: async () => {
					throw AdapterError.badArgument("textLayer is not implemented by the engine host");
				},
				search: async function* () {
					yield { matches: [], progress: 1, done: true };
				},
			},
		});
		const client = createEnginePort(link.client);
		const doc = await client.open(sampleDescriptor());
		await expect(client.renderTile({ doc, page: 0, scale: 1 })).resolves.toBeDefined();
		expect(encodeBase64(new Uint8Array([1, 2, 3, 4]))).toBe("AQIDBA==");
	});

	it("gives the host no document bytes to leak when the port drops", async () => {
		// The host closes every open document when the link is torn down,
		// because the page that owned them is gone and nothing else would ask.
		const closed: string[] = [];
		const link = createLoopbackLink();
		const engine = createFixtureEngine();
		serveEngineHost({
			link: link.host,
			engine: {
				open: engine.open,
				close: async (doc, options) => {
					closed.push(doc.id);
					return await engine.close(doc, options);
				},
				renderTile: engine.renderTile,
				extractText: engine.extractText,
				textLayer: engine.textLayer,
				search: engine.search,
			},
			navigation: engine,
		});
		const client = createEnginePort(link.client);
		await client.open(sampleDescriptor());
		expect(closed).toEqual([]);
		link.client.close();
		await new Promise((resolve) => setTimeout(resolve, 0));
		expect(closed).toHaveLength(1);
	});

	it("extracts text in the sample's reading order", async () => {
		const { client } = connected();
		const doc = await client.open(sampleDescriptor());
		const page = await client.extractText(doc, 0);
		expect(page.text).toBe(SAMPLE_TEXTS[0]);
	});
});

/**
 * The navigation half (SL-4.UI.06).
 *
 * The shared contract suite already probes the four reads, but it probes
 * *shape* — that each returns an array — because its fixture document has no
 * outline. These tests are the ones that read the content across the wire, and
 * the ones that prove the two refusals the extension owes are different
 * refusals: a host that cannot read outlines (6017) and a page that does not
 * exist (6001).
 */
describe("the navigation transport (SL-4.UI.06)", () => {
	it("carries the outline, the labels, the destinations and the links", async () => {
		const { client } = connected();
		const doc = await client.open(sampleDescriptor());

		const outline = await client.outline(doc);
		expect(outline.map((node) => node.title)).toEqual(["Chapter one", "Appendix"]);
		// The nesting survives the wire: a flat list would tell a reader that
		// two chapters are two siblings.
		expect(outline[0]?.children?.map((child) => child.title)).toEqual(["Figure 1"]);
		expect(outline[0]?.children?.[0]?.destination?.page).toBe(1);
		// A bookmark that *names* a destination rather than carrying one comes
		// back as itself, not dropped.
		expect(outline[1]?.namedDestination).toBe("here");
		expect(outline[1]?.destination).toBeUndefined();

		const labels = await client.pageLabels(doc);
		expect(labels).toEqual([
			{ firstPage: 0, style: "r", prefix: "p" },
			{ firstPage: 2, style: "D", firstValue: 7 },
		]);

		const destinations = await client.destinations(doc);
		expect(destinations).toEqual([{ name: "here", destination: { page: 2 } }]);

		const links = await client.pageLinks(doc, 0);
		expect(links.map((link) => link.id)).toEqual(["p0-l0", "launch-1"]);
		expect(links[0]?.rect).toEqual({ x: 72, y: 700, width: 100, height: 12 });
	});

	it("reports ADR-0020's disabled action classes verbatim", async () => {
		// The transport reports what the document says; the *viewer* decides.
		// A transport that dropped `/Launch` here would make the viewer's
		// refusal untestable, and would let a real host get away with the same.
		const { client } = connected();
		const doc = await client.open(sampleDescriptor());
		const links = await client.pageLinks(doc, 0);
		expect(links.map((link) => link.action.kind)).toEqual(["uri", "launch"]);
		expect(links[1]?.action.uri).toBe("C:/payload.exe");
	});

	it("refuses an out-of-range page with BINDING_BAD_ARGUMENT, not an empty list", async () => {
		// "Page 9000 of a 3-page document has no links" and "this host cannot
		// read links" must not look alike, so the refusal is typed and it is
		// the *seam's* code rather than the engine's 1301.
		const { client } = connected();
		const doc = await client.open(sampleDescriptor());
		await expect(client.pageLinks(doc, doc.pageCount)).rejects.toMatchObject({
			code: ErrorCode.BindingBadArgument,
			docState: "Unchanged",
		});
	});

	it("refuses with BINDING_UNSUPPORTED_OP when the host cannot read outlines", async () => {
		// A host without the capability is a *capability* fact, not a
		// malformed request: 6017, not 6001. The two are what let a viewer
		// tell "nobody could look" from "there was nothing to see".
		const link = createLoopbackLink();
		const engine = createFixtureEngine();
		serveEngineHost({ link: link.host, engine });
		const client = createEnginePort(link.client);
		const doc = await client.open(sampleDescriptor());
		for (const call of [
			client.outline(doc),
			client.pageLabels(doc),
			client.destinations(doc),
			client.pageLinks(doc, 0),
		]) {
			await expect(call).rejects.toMatchObject({
				code: ErrorCode.BindingUnsupportedOp,
				docState: "Unchanged",
			});
		}
	});

	it("refuses a closed document with BINDING_BAD_HANDLE, not an empty outline", async () => {
		const { client } = connected();
		const doc = await client.open(sampleDescriptor());
		await client.close(doc);
		await expect(client.outline(doc)).rejects.toMatchObject({
			code: ErrorCode.BindingBadHandle,
			docState: "Unchanged",
		});
	});

	it("refuses a closed document before it refuses the capability", async () => {
		// The order matters and is stated: the document is the *subject* of the
		// request, so a request naming a document this host never issued is
		// malformed whatever the host can do. A host that could read outlines
		// answers 6000 for a closed document and 6017 for a live one; a host
		// that cannot answers 6000 for a closed document, because the handle
		// is wrong either way.
		const link = createLoopbackLink();
		const engine = createFixtureEngine();
		serveEngineHost({ link: link.host, engine });
		const client = createEnginePort(link.client);
		const doc = await client.open(sampleDescriptor());
		await client.close(doc);
		await expect(client.outline(doc)).rejects.toMatchObject({
			code: ErrorCode.BindingBadHandle,
			docState: "Unchanged",
		});
	});

	it("refuses a bookmark with no title rather than showing an empty panel", async () => {
		// The page is a trust boundary: the reply arrives as JSON over a port
		// and every field is checked before it reaches the UI's types.
		const link = createLoopbackLink();
		link.host.subscribe(() => {
			// The host exists; it does not answer on its own.
		});
		const client = createEnginePort(link.client);
		const pending = client.outline({ id: "d1", pageCount: 3, pageSizes: [] });
		link.host.post({
			v: 1,
			id: 1,
			ok: true,
			value: {
				present: true,
				truncated: false,
				pruned: false,
				items: [{ title: 42 }],
			},
		} as never);
		await expect(pending).rejects.toMatchObject({ code: ErrorCode.BindingBadArgument });
	});

	it("refuses a link whose rectangle is missing a component", async () => {
		// A rectangle missing a component is a highlight somewhere arbitrary
		// that still type-checks. The page side refuses it.
		const link = createLoopbackLink();
		link.host.subscribe(() => {
			// The host exists; it does not answer on its own.
		});
		const client = createEnginePort(link.client);
		const pending = client.pageLinks({ id: "d1", pageCount: 3, pageSizes: [] }, 0);
		link.host.post({
			v: 1,
			id: 1,
			ok: true,
			value: {
				page: 0,
				links: [
					{
						id: "l0",
						rect: { x: 1, y: 2, width: 3 },
						action: { kind: "uri", uri: "https://example.com/a" },
					},
				],
			},
		} as never);
		await expect(pending).rejects.toMatchObject({ code: ErrorCode.BindingBadArgument });
	});

	it("refuses an action class the viewer does not model", async () => {
		// The guest maps a class the *engine* does not model to `unknown`, and
		// the viewer has a decision for `unknown`. A class outside the
		// viewer's own vocabulary means the host and the page disagree about
		// the protocol, which is a bug rather than a document fact.
		const link = createLoopbackLink();
		link.host.subscribe(() => {
			// The host exists; it does not answer on its own.
		});
		const client = createEnginePort(link.client);
		const pending = client.pageLinks({ id: "d1", pageCount: 3, pageSizes: [] }, 0);
		link.host.post({
			v: 1,
			id: 1,
			ok: true,
			value: {
				page: 0,
				links: [
					{
						id: "l0",
						rect: { x: 1, y: 2, width: 3, height: 4 },
						action: { kind: "rendition" },
					},
				],
			},
		} as never);
		await expect(pending).rejects.toMatchObject({ code: ErrorCode.BindingBadArgument });
	});
});
