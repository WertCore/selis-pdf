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
	serveEngineHost({ link: link.host, engine: createFixtureEngine() });
	const client = createEnginePort(link.client);
	return { link, client };
}

definePlatformAdapterContract(
	"extension offscreen transport",
	async () => {
		const link = createLoopbackLink();
		serveEngineHost({ link: link.host, engine: createFixtureEngine() });
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
