/**
 * Browser harness leg for FONT.10's web half (SL-3.FONT.10).
 *
 * The Rust text chunk now emits a typed CJK subset-substitution disclosure
 * (`selis-font-substitution/1`) alongside the extracted text. This test is the
 * browser-side counterpart: it takes that disclosure, resolves the on-demand
 * chunk it names through {@link CjkCacheSource} over an in-memory Cache Storage
 * double, and drives the real {@link LazyPayloadClient.drainCjk} flow against a
 * fake guest. It proves the browser caller receives the disclosure *and* can
 * fulfil the chunk it names from a cache without ever uploading document bytes.
 *
 * No font payload is embedded in this test: the bytes are a three-byte
 * stand-in for a chunk the guest already claimed, exactly like `lazy-payload`'s
 * own fakes. The disclosure itself is the data under test here.
 */

import { describe, expect, it } from "vitest";

import {
	CjkCacheSource,
	cacheStoragePayloadCache,
	type CacheStorageLike,
} from "./cjk-cache-source.js";
import {
	CJK_DESCRIPTOR,
	LazyPayloadClient,
	type LazyTransport,
	payloadCacheKey,
} from "./lazy-payload.js";

/** The exact JSON shape the wasm text chunk emits (see its `font10_*` test). */
const DISCLOSURE = `{
  "schema": "selis-font-substitution/1",
  "notes": [
    {
      "requested_base_font": "STSong-Light",
      "code": 20013,
      "chunk": "ideographs-1",
      "subset": true,
      "disclosure": "font STSong-Light substituted with CJK on-demand subset chunk ideographs-1 for U+4E2D"
    }
  ]
}`;

interface Note {
	readonly requested_base_font: string;
	readonly code: number;
	readonly chunk: string | null;
	readonly subset: boolean;
	readonly disclosure: string;
}

function disclosure(): Note {
	const parsed = JSON.parse(DISCLOSURE) as { schema: string; notes: Note[] };
	expect(parsed.schema).toBe("selis-font-substitution/1");
	const note = parsed.notes[0];
	if (!note) {
		throw new Error("no disclosure note");
	}
	return note;
}

/** A minimal Response double — Cache Storage holds responses, not raw arrays. */
class FakeResponse {
	readonly status: number;
	readonly #bytes: Uint8Array;

	constructor(bytes: Uint8Array, init: { status?: number } = {}) {
		this.#bytes = bytes;
		this.status = init.status ?? 200;
	}

	async arrayBuffer(): Promise<ArrayBuffer> {
		return this.#bytes.slice().buffer as ArrayBuffer;
	}
}

/** An in-memory Cache Storage double, injected — no platform global in the adapter. */
function cacheDouble(): CacheStorageLike & { readonly keys: () => string[] } {
	const stores = new Map<string, Map<string, unknown>>();
	return {
		keys: () => [...(stores.get("selis-cjk-v1")?.keys() ?? [])],
		async open(name) {
			let store = stores.get(name);
			if (!store) {
				store = new Map();
				stores.set(name, store);
			}
			return {
				async match(key) {
					return store?.get(key);
				},
				async put(key, value) {
					store?.set(key, value);
				},
			};
		},
	};
}

/** A guest script for one `cjkOpen` and one `cjkChunk` delivery. */
function fakeGuest(adoptedId: string): LazyTransport {
	let at = 0;
	const steps: Array<{ op: string; value: Record<string, unknown> }> = [
		{
			op: "cjkOpen",
			value: {
				revision: 0,
				residentBytes: 0,
				needs: [],
				unavailable: [],
				exhausted: [],
				request: null,
				loaded: [],
			},
		},
		{
			op: "cjkChunk",
			value: {
				revision: 1,
				residentBytes: 3,
				needs: [],
				unavailable: [],
				exhausted: [],
				request: null,
				loaded: [adoptedId],
				adopted: true,
			},
		},
	];
	return {
		async call(op, _body, _attachment) {
			const step = steps[at];
			if (!step || step.op !== op) {
				throw new Error(`unexpected guest op ${op} at ${at}`);
			}
			at += 1;
			return step.value;
		},
	};
}

describe("FONT.10 web half — typed disclosure drives a cached CJK chunk", () => {
	it("the disclosure names an on-demand subset chunk, not an embedded font", () => {
		const note = disclosure();
		expect(note.requested_base_font).toBe("STSong-Light");
		expect(note.code).toBe(0x4e2d);
		expect(note.chunk).toBe("ideographs-1");
		expect(note.subset).toBe(true);
		expect(note.disclosure).toContain("on-demand subset chunk");
	});

	it("drainCjk fulfils the named chunk through the cache adapter", async () => {
		const note = disclosure();
		const sha256 = "d".repeat(64);
		const claim = {
			id: note.chunk as string,
			sha256,
			rawBytes: 3,
			url: `cjk/${note.chunk}.ttf`,
		};
		const storage = cacheDouble();
		const cache = cacheStoragePayloadCache(storage, "selis-cjk-v1", FakeResponse);
		let fetches = 0;
		const source = new CjkCacheSource<string>({
			cache,
			async fetchBytes() {
				fetches += 1;
				return { status: 200, bytes: new Uint8Array([1, 2, 3]) };
			},
		});
		const client = new LazyPayloadClient(CJK_DESCRIPTOR, fakeGuest(claim.id), source);
		await client.open(1, [claim], { core: new Uint8Array(1) });
		const result = await client.drainCjk(1, {
			needs: [claim.id],
			revision: 0,
			residentBytes: 0,
		});

		expect(result.delivered).toEqual([claim.id]);
		expect(fetches).toBe(1);
		// The cache key is derived from the digest, never from a document byte.
		const key = payloadCacheKey("cjk", claim.id, sha256);
		expect(storage.keys()).toEqual([key]);
		expect(key).toContain(claim.id);
		expect(key).toContain(sha256);
		expect(key).not.toContain("STSong-Light");
		expect(key).not.toContain("4E2D");

		// A second pass with the same claim is served from Cache Storage: the
		// guest is still told to repaint, but no second network fetch happens.
		const second = new CjkCacheSource<string>({
			cache,
			async fetchBytes() {
				fetches += 1;
				return { status: 200, bytes: new Uint8Array([9]) };
			},
		});
		const cached = await second.load(claim, key);
		expect(cached.status).toBe(200);
		expect([...cached.bytes]).toEqual([1, 2, 3]);
		expect(fetches).toBe(1);
	});

	it("refuses a key that does not match the guest's digest claim", async () => {
		const note = disclosure();
		const storage = cacheDouble();
		const source = new CjkCacheSource<string>({
			cache: cacheStoragePayloadCache(storage, "selis-cjk-v1", FakeResponse),
			async fetchBytes() {
				throw new Error("must not fetch for a mismatched key");
			},
		});
		const delivery = await source.load(
			{
				id: note.chunk as string,
				sha256: "a".repeat(64),
				rawBytes: 3,
				url: "cjk/ideographs-1.ttf",
			},
			payloadCacheKey("cjk", note.chunk as string, "b".repeat(64)),
		);
		expect(delivery.status).toBe(0);
		expect(storage.keys()).toEqual([]);
	});
});
