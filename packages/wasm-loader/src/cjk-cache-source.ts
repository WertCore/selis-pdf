/**
 * Cache Storage adapter for lazy CJK subset chunks (SL-3.FONT.10).
 *
 * ## Why this module exists
 *
 * `LazyPayloadClient` is a pure client over two injected ports: a
 * {@link LazyTransport} and a {@link LazyByteSource}. Until now the repository
 * had no source that actually *caches* a chunk — the service worker cached the
 * shell and the wasm binary only — so the "cache them" half of FONT.10 was a
 * design note rather than code. This module supplies the missing source:
 *
 * * {@link CjkCacheSource} is a {@link LazyByteSource} that looks up each
 *   request under the digest-derived key the client hands it
 *   ({@link payloadCacheKey}), fetches through an injected port on a miss, and
 *   stores the bytes for the next pass. It never derives a URL, never uploads
 *   a document and never decides whether a body is good — the guest still does
 *   that, exactly as `LazyPayloadClient` documents.
 * * {@link cacheStoragePayloadCache} binds that port to a Cache Storage-like
 *   object. The object is **injected**, not the `caches` global: ADR-P0044 keeps
 *   platform globals out of this package, and injection is what makes the
 *   adapter testable against an in-memory double in `cjk-cache-source.test.ts`.
 *
 * No font bytes are embedded here. The adapter moves bytes the guest already
 * named, under a key that includes the SHA-256 the guest will verify.
 */

import {
	payloadCacheKey,
	type ClaimBody,
	type LazyByteSource,
	type PayloadDelivery,
} from "./lazy-payload.js";

/** The bytes-only surface a cache must provide to the adapter. */
export interface PayloadCache {
	/** The decompressed bytes stored under `key`, or `undefined` on a miss. */
	get(key: string): Promise<Uint8Array | undefined>;
	/** Store decompressed bytes under `key`. */
	put(key: string, bytes: Uint8Array): Promise<void>;
}

/** An injected byte fetcher — a URL in, a status and bytes out, nothing else. */
export type FetchBytes = (url: string) => Promise<PayloadDelivery>;

/** Options for {@link CjkCacheSource}. */
export interface CjkCacheSourceOptions {
	/** The cache that holds decompressed chunk bytes, keyed by digest. */
	cache: PayloadCache;
	/** The network port used on a cache miss. */
	fetchBytes: FetchBytes;
}

/**
 * A {@link LazyByteSource} that serves CJK chunks from a cache first.
 *
 * The `key` argument is authoritative: it is `payload:id:digest`, so a
 * re-pinned digest cannot be served the old bytes under a new name. A cached
 * entry is returned with status 200 (the guest verifies the digest itself, so
 * the cache is never trusted to have validated the body). A miss delegates to
 * the injected fetcher and stores only a 200 response — statuses the guest
 * treats as retry/give-up are left uncached, preserving its attempt accounting.
 */
export class CjkCacheSource<Id extends string> implements LazyByteSource<Id> {
	readonly #cache: PayloadCache;
	readonly #fetchBytes: FetchBytes;

	constructor(options: CjkCacheSourceOptions) {
		this.#cache = options.cache;
		this.#fetchBytes = options.fetchBytes;
	}

	async load(request: ClaimBody<Id>, key: string): Promise<PayloadDelivery> {
		// If a caller ever hands a key that is not the client's digest key,
		// refusing beats serving a chunk under a namespace the guest did not
		// name. `payloadCacheKey` is exported precisely so the client and this
		// source derive the same string from the same claim.
		const expected = payloadCacheKey("cjk", request.id, request.sha256);
		if (key !== expected) {
			return { status: 0, bytes: new Uint8Array(0) };
		}
		const cached = await this.#cache.get(key);
		if (cached !== undefined) {
			return { status: 200, bytes: cached };
		}
		const delivered = await this.#fetchBytes(request.url);
		if (delivered.status === 200) {
			await this.#cache.put(key, delivered.bytes);
		}
		return delivered;
	}
}

/**
 * The minimal Cache Storage surface {@link cacheStoragePayloadCache} needs.
 * Modelled on the real API but injected, so this package never names `caches`.
 */
export interface CacheStorageLike {
	open(name: string): Promise<{
		match(key: string): Promise<unknown>;
		put(key: string, value: unknown): Promise<unknown>;
	}>;
}

/**
 * Bind a Cache Storage-like object to the bytes-only {@link PayloadCache} port.
 *
 * The stored value is a `Response` because a real Cache Storage holds responses,
 * not bare arrays; the constructor is injected for the same reason the storage
 * is — this module stays global-free and the test can supply a small double.
 * Only the digest-derived key is used as the request: no document bytes, no
 * reconstructed URL, and no state other than the chunk the guest named.
 */
export function cacheStoragePayloadCache(
	storage: CacheStorageLike,
	name: string,
	ResponseCtor: new (body: Uint8Array, init?: { status?: number }) => {
		readonly status: number;
		arrayBuffer(): Promise<ArrayBuffer>;
	},
): PayloadCache {
	return {
		async get(key) {
			const hit = await (await storage.open(name)).match(key);
			if (!hit || typeof (hit as { arrayBuffer?: unknown }).arrayBuffer !== "function") {
				return undefined;
			}
			const response = hit as { status: number; arrayBuffer(): Promise<ArrayBuffer> };
			if (response.status !== 200) {
				return undefined;
			}
			return new Uint8Array(await response.arrayBuffer());
		},
		async put(key, bytes) {
			await (await storage.open(name)).put(key, new ResponseCtor(bytes, { status: 200 }));
		},
	};
}
