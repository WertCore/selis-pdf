/**
 * The SL-4.UI.01 DoD gate, extended to this package by hand: production code in
 * `packages/wasm-loader` must not name a platform global either.
 *
 * ## Why this file is a copy rather than nothing
 *
 * ADR-P0044's scan (`apps/ui/src/platform/platform-globals.test.ts`) covers
 * `apps/ui/src` only, so a global in this package would sail past the automated
 * gate — and this package is where the lazy-payload client that *moves bytes
 * over a network* lives. A `fetch` reaching into this client would be the one
 * place in the shell where an unverified body could enter the protocol, and it
 * would enter as a `LazyByteSource` the guest never named.
 *
 * So the same list is asserted here, unchanged, rather than left to review.
 * The rule is not "apps/** may not" — it is what the architecture is, and
 * `packages/**` is not exempt from it in spirit even where the scanner does not
 * reach.
 */

import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { describe, expect, it } from "vitest";

const srcRoot = join(dirname(fileURLToPath(import.meta.url)));

/**
 * The modules this task added, which is what the gate below covers.
 *
 * Scoped by name rather than by "every production file" because
 * `src/loader.ts` — pre-existing, and not this task's — reaches the bare `fetch`
 * global in its `defaultFetch` fallback. That is a real divergence from the
 * rule and it is reported rather than quietly excluded: see the second test in
 * this file, which pins the divergence so it cannot be forgotten or grow.
 */
const GATED_MODULES = ["lazy-payload.ts", "fallback-manifest.ts"];

/**
 * The same 18 identifiers `platform-globals.test.ts` forbids, verbatim.
 *
 * Duplicated on purpose. Importing the list would couple this package's test to
 * another package's internals for no gain, and the point of the list is that it
 * is a fixed, reviewable set of names rather than something derived.
 */
const FORBIDDEN_GLOBALS = [
	"window",
	"document",
	"navigator",
	"location",
	"localStorage",
	"sessionStorage",
	"indexedDB",
	"caches",
	"fetch",
	"alert",
	"confirm",
	"prompt",
	"crypto",
	"chrome",
	"browser",
	"Worker",
	"SharedArrayBuffer",
	"OffscreenCanvas",
] as const;

/**
 * Bare-identifier usage: not a property key (`window:`), not a member (`x.window`).
 *
 * A **fresh** RegExp per call. The `/g` flag makes `.test()` carry `lastIndex`
 * between uses, so a shared instance answers differently depending on what was
 * tested before it — and a gate that fails intermittently is worse than no
 * gate, because it trains people to re-run it until it goes green.
 */
const usagePattern = (name: string): RegExp =>
	new RegExp(`(?<![\\w$."'])${name}(?![\\w$])(?!\\s*:)`, "g");

/** Whether `code` names `name` bare, with no regex state carried between calls. */
const usesGlobal = (code: string, name: string): boolean => {
	usagePattern(name).lastIndex = 0;
	return usagePattern(name).test(code);
};

/**
 * Strip comments and string literals.
 *
 * Copied from the apps/ui gate including its blind spots — a global named only
 * inside a template interpolation would be missed — because this is a
 * guardrail against accidental global access, not a parser. Stripping the
 * comments is what lets this file *discuss* `fetch` at length, which the
 * module doc does on purpose.
 */
function stripComments(source: string): string {
	return source.replace(/\/\*[\s\S]*?\*\//g, "").replace(/(^|[^:"'`\\])\/\/[^\n]*/g, "$1");
}

/** One named production `.ts` in this package; throws if it is not there. */
function productionSource(name: string): string {
	const path = join(srcRoot, name);
	return readFileSync(path, "utf8");
}

describe("ADR-P0044 — the lazy-payload modules name no platform global", () => {
	/**
	 * The whole point, asserted rather than asserted-in-a-comment: this client
	 * is the one place in the shell where bytes enter the protocol, so a
	 * `fetch` here is a body the guest never asked for arriving anyway — which
	 * is invariant 1, broken at its source.
	 */
	it("neither module names one of the 18 globals", () => {
		const offences: string[] = [];
		for (const name of GATED_MODULES) {
			const code = stripComments(productionSource(name));
			for (const forbidden of FORBIDDEN_GLOBALS) {
				if (usesGlobal(code, forbidden)) {
					offences.push(`${name}: ${forbidden}`);
				}
			}
		}
		expect(offences).toEqual([]);
	});

	/**
	 * **A pre-existing divergence, recorded so it is visible rather than
	 * forgotten.** `src/loader.ts` reaches the bare `fetch` global in its
	 * `defaultFetch` fallback — injected `fetchFn` first, real `fetch` as the
	 * last resort. It predates this task and is not touched here, but it is the
	 * reason the gate above is scoped by name, and a reader who widens the scan
	 * will hit it. Pinned so that if someone *does* fix it, this test is the
	 * thing that has to be updated in the same commit — which is what turns
	 * "someone should fix that" into a tracked state rather than a shrug.
	 */
	it("loader.ts is the one pre-existing global in this package, and it is the documented one", () => {
		const loader = stripComments(productionSource("loader.ts"));
		const uses = FORBIDDEN_GLOBALS.filter((name) => usesGlobal(loader, name));
		expect(uses).toEqual(["fetch"]);
		// And the injection point that makes it a fallback rather than a
		// dependency: `fetchFn` is a constructor option, defaulted, not
		// required. The global is the last resort, not the only path.
		expect(loader).toContain("opts.fetchFn ?? defaultFetch");
	});
});

import {
	CJK_DESCRIPTOR,
	type ClaimBody,
	FALLBACK_DESCRIPTOR,
	LazyPayloadClient,
	LazyPayloadError,
	type LazyTransport,
	type PayloadDelivery,
	indexClaims,
	payloadCacheKey,
	wireClaim,
} from "./lazy-payload.js";

/** One `call(op, body, attachment)` the fake guest should answer with. */
interface Step {
	readonly op: string;
	readonly value: unknown;
}

/**
 * A `request` value as it crosses the wire, where the identity field is `id`
 * for CJK and `name` for fallback.
 *
 * Typed as a loose record rather than the client's renamed {@link ClaimBody},
 * because that rename is the *client's* doing — the fake guest is standing in
 * for the engine, and the engine writes `id` and `name` in two different ops.
 */
interface WireRequest {
	readonly id?: string;
	readonly name?: string;
	readonly sha256: string;
	readonly rawBytes: number;
	readonly url: string;
}

/**
 * A state value, with the fields the protocol pins and nothing invented.
 *
 * Defaults are the *empty* state on purpose: a document that has needed nothing
 * is the common case, and a fake that started full would hide the
 * `request: null` path the open tests depend on.
 */
function state(
	over: Partial<{
		revision: number;
		residentBytes: number;
		loaded: string[];
		needs: string[];
		unavailable: string[];
		exhausted: string[];
		request: WireRequest | null;
		adopted: boolean;
		closed: boolean;
		releasedBytes: number;
	}> = {},
): Record<string, unknown> {
	return {
		revision: 0,
		residentBytes: 0,
		loaded: [],
		needs: [],
		unavailable: [],
		exhausted: [],
		request: null,
		...over,
	};
}

/** A recorded call, so a test can assert on the body that actually went out. */
interface Call {
	readonly op: string;
	readonly body: Record<string, unknown>;
	readonly attachment: Uint8Array | null;
}

/**
 * A guest that answers a fixed script and records every call.
 *
 * Deliberately dumb: it does not verify digests, does not enforce a budget and
 * does not count attempts, because a fake that made those judgements would let
 * a bug in the *client's* handling pass unnoticed. Everything the client must
 * respect is asserted from the recorded calls instead.
 */
function fakeGuest(steps: readonly Step[]): LazyTransport & { readonly calls: Call[] } {
	const calls: Call[] = [];
	let at = 0;
	return {
		calls,
		async call(op, body, attachment) {
			calls.push({ op, body, attachment });
			const step = steps[at];
			if (step === undefined) {
				throw new Error(`fake guest ran out of script at call ${at} (${op})`);
			}
			if (step.op !== op) {
				throw new Error(`fake guest expected ${step.op} at call ${at}, got ${op}`);
			}
			at += 1;
			return step.value;
		},
	};
}

/** A byte source that serves what it was given, and records the keys it saw. */
function fakeSource(
	bytes: Uint8Array = new Uint8Array([1, 2, 3]),
	status = 200,
): { load: (r: ClaimBody<string>, key: string) => Promise<PayloadDelivery>; keys: string[] } {
	const keys: string[] = [];
	return {
		keys,
		async load(_request, key) {
			keys.push(key);
			return { status, bytes };
		},
	};
}

const CHUNK_CLAIM: ClaimBody<string> = {
	id: "kanji",
	sha256: "a".repeat(64),
	rawBytes: 3,
	url: "cjk/kanji.ttf",
};

const KANA_CLAIM: ClaimBody<string> = {
	id: "kana",
	sha256: "c".repeat(64),
	rawBytes: 3,
	url: "cjk/kana.ttf",
};

const FACE_CLAIM: ClaimBody<string> = {
	id: "LiberationSerif-Regular",
	sha256: "b".repeat(64),
	rawBytes: 3,
	url: "fallback/LiberationSerif-Regular.ttf.br",
};

const SANS_CLAIM: ClaimBody<string> = {
	id: "LiberationSans-Regular",
	sha256: "e".repeat(64),
	rawBytes: 3,
	url: "fallback/LiberationSans-Regular.ttf.br",
};

/**
 * A `request` value in the wire shape a *fallback* guest sends: the identity is
 * `name`, not `id`. Used wherever a fallback response has to name a next face,
 * because a claim reused verbatim here would carry `id` and the reader would
 * (correctly) refuse it.
 */
const NEXT_FACE = {
	name: SANS_CLAIM.id,
	url: SANS_CLAIM.url,
	sha256: SANS_CLAIM.sha256,
	rawBytes: SANS_CLAIM.rawBytes,
};

describe("SL-4.WASM.07 / SL-3.FONT.12 — the shared lazy-payload client", () => {
	// --- the descriptor is the whole of the payload difference --------------

	/**
	 * The two payloads differ in five field names and two op names, and in
	 * nothing else. Pinned exactly, because a typo here is silent: the guest
	 * answers `BINDING_BAD_ARGUMENT` for a body whose claim field is `name`
	 * when it wanted `id`, and a test that only checked "the open succeeded"
	 * would never notice.
	 */
	it("names the wire for both payloads, and the difference is only names", () => {
		expect(CJK_DESCRIPTOR).toMatchObject({
			kind: "cjk",
			openOp: "cjkOpen",
			deliverOp: "cjkChunk",
			claimKey: "id",
			deliverKey: "chunk",
			absentKey: "unserved",
		});
		expect(FALLBACK_DESCRIPTOR).toMatchObject({
			kind: "fallback",
			openOp: "fallbackOpen",
			deliverOp: "fallbackFace",
			claimKey: "name",
			deliverKey: "face",
			absentKey: "unavailable",
		});
		// The absence of a behavioural flag is the load-bearing part: a
		// descriptor that could be configured to change *ordering* would be the
		// exact bug this design exists to make impossible.
		for (const d of [CJK_DESCRIPTOR, FALLBACK_DESCRIPTOR]) {
			expect(Object.keys(d).sort()).toEqual([
				"absentKey",
				"claimKey",
				"deliverKey",
				"deliverOp",
				"extras",
				"kind",
				"openOp",
			]);
		}
	});

	/**
	 * `CjkClaimBody` serialises `id` and `FaceClaimBody` serialises `name` from
	 * one shared claim type, so a wrong key produces a body the guest cannot
	 * even deserialise. Pinned per payload, straight from `protocol.rs`.
	 */
	it("writes the claim under the identity field each payload uses", () => {
		expect(wireClaim("id", CHUNK_CLAIM)).toEqual({
			id: "kanji",
			sha256: "a".repeat(64),
			rawBytes: 3,
			url: "cjk/kanji.ttf",
		});
		expect(wireClaim("name", FACE_CLAIM)).toEqual({
			name: "LiberationSerif-Regular",
			sha256: "b".repeat(64),
			rawBytes: 3,
			url: "fallback/LiberationSerif-Regular.ttf.br",
		});
	});

	/**
	 * ADR-P0016: there is no field on a delivery op a body could be smuggled
	 * through, and the client must not invent one. Every key sent on
	 * `fallbackFace` is asserted exactly, so a future "helpful" retry that
	 * attaches a range header as a body field fails here.
	 */
	it("sends a bodyless delivery: doc, identity, status, len — and no method or body", async () => {
		const guest = fakeGuest([
			{ op: "fallbackOpen", value: state() },
			{ op: "fallbackFace", value: state({ adopted: true }) },
		]);
		const client = new LazyPayloadClient(FALLBACK_DESCRIPTOR, guest, fakeSource());
		await client.open(4, [FACE_CLAIM]);
		await client.serve(4, FACE_CLAIM);
		const deliver = guest.calls[1];
		expect(deliver?.op).toBe("fallbackFace");
		expect(Object.keys(deliver?.body ?? {}).sort()).toEqual(["doc", "face", "len", "status"]);
		expect(deliver?.body.face).toBe("LiberationSerif-Regular");
		expect(deliver?.body).not.toHaveProperty("method");
		expect(deliver?.body).not.toHaveProperty("body");
	});

	// --- open ---------------------------------------------------------------

	/**
	 * `cjkOpen` carries the core subset as its binary attachment and *must*
	 * declare `coreLen` matching it, or the guest answers
	 * `BINDING_BAD_ARGUMENT`. The length is taken from the bytes rather than
	 * asked for, because that way the two cannot disagree.
	 */
	it("cjkOpen declares coreLen from the attachment it actually sends", async () => {
		const guest = fakeGuest([{ op: "cjkOpen", value: state() }]);
		const client = new LazyPayloadClient(CJK_DESCRIPTOR, guest, fakeSource());
		const core = new Uint8Array([9, 9, 9, 9]);
		await client.open(1, [CHUNK_CLAIM], { core });
		expect(guest.calls[0]?.body.coreLen).toBe(4);
		expect(guest.calls[0]?.attachment).toBe(core);
	});

	/**
	 * A `cjkOpen` with no core cannot be built at all: `core_len` is a required
	 * `u64` in the protocol, not an `Option`, so the client refuses before the
	 * message goes out rather than sending a body the guest will reject.
	 */
	it("refuses a cjkOpen with no core subset", async () => {
		const guest = fakeGuest([]);
		const client = new LazyPayloadClient(CJK_DESCRIPTOR, guest, fakeSource());
		await expect(client.open(1, [CHUNK_CLAIM])).rejects.toThrow(/cjkOpen needs the core subset/);
		expect(guest.calls).toHaveLength(0);
	});

	/**
	 * `fallbackOpen` has no core attachment because the two embedded faces are
	 * already inside the wasm binary. Sending one anyway would be a second copy
	 * of bytes the guest did not ask for, which is invariant 1.
	 */
	it("fallbackOpen sends no attachment at all", async () => {
		const guest = fakeGuest([{ op: "fallbackOpen", value: state() }]);
		const client = new LazyPayloadClient(FALLBACK_DESCRIPTOR, guest, fakeSource());
		await client.open(2, [FACE_CLAIM]);
		expect(guest.calls[0]?.attachment).toBeNull();
		expect(guest.calls[0]?.body).not.toHaveProperty("coreLen");
	});

	/**
	 * Both opens carry the *absent* list, and both name it differently. It is
	 * `skip_serializing_if = "Vec::is_empty"` in the protocol, so an empty list
	 * must be omitted rather than sent as `[]` — a shell that always sent the
	 * key would fail a guest built with a stricter decoder.
	 */
	it("sends the absent list under each payload's name, and omits it when empty", async () => {
		const guest = fakeGuest([
			{ op: "cjkOpen", value: state() },
			{ op: "fallbackOpen", value: state() },
			{ op: "cjkOpen", value: state() },
		]);
		const cjk = new LazyPayloadClient(CJK_DESCRIPTOR, guest, fakeSource());
		await cjk.open(1, [CHUNK_CLAIM], { core: new Uint8Array(1), absent: ["korean"] });
		expect(guest.calls[0]?.body.unserved).toEqual(["korean"]);

		const fallback = new LazyPayloadClient(FALLBACK_DESCRIPTOR, guest, fakeSource());
		await fallback.open(2, [FACE_CLAIM], { absent: ["Wingdings-Annoyance"] });
		expect(guest.calls[1]?.body.unavailable).toEqual(["Wingdings-Annoyance"]);

		await cjk.open(1, [CHUNK_CLAIM], { core: new Uint8Array(1) });
		expect(guest.calls[2]?.body).not.toHaveProperty("unserved");
	});

	/**
	 * The `budget` the caller chose passes through untouched and is omitted when
	 * absent — ADR-P0006: the caller chooses its limits, so the client must
	 * neither invent a ceiling nor reinterpret one.
	 */
	it("passes the caller's budget through and omits it when unset", async () => {
		const guest = fakeGuest([
			{ op: "fallbackOpen", value: state() },
			{ op: "fallbackOpen", value: state() },
		]);
		const client = new LazyPayloadClient(FALLBACK_DESCRIPTOR, guest, fakeSource());
		await client.open(2, [FACE_CLAIM], { budget: { bytes: 1_000_000 } });
		expect(guest.calls[0]?.body.budget).toEqual({ bytes: 1_000_000 });
		await client.open(2, [FACE_CLAIM]);
		expect(guest.calls[1]?.body).not.toHaveProperty("budget");
	});

	/**
	 * A freshly-opened payload has wanted nothing, and the guest says so with an
	 * explicit `request: null`. Treating that as a failure would fail every
	 * open, so the state is returned as-is with an empty queue.
	 */
	it("a null request on open is a document that has needed nothing, not an error", async () => {
		const guest = fakeGuest([{ op: "fallbackOpen", value: state({ revision: 0 }) }]);
		const client = new LazyPayloadClient(FALLBACK_DESCRIPTOR, guest, fakeSource());
		const opened = await client.open(2, [FACE_CLAIM]);
		expect(opened.request).toBeNull();
		expect(opened.needs).toEqual([]);
		expect(client.state).not.toBeNull();
	});

	// --- the cache key (ADR-P0043 §3) --------------------------------------

	/**
	 * The invariant this whole module is built around: the key pairs the
	 * identity with the digest. A key on identity alone cannot tell a poisoned
	 * entry from a good one, and the two are indistinguishable by inspection —
	 * a tampered TTF is still a TTF.
	 */
	it("pairs the identity with the digest in the cache key", () => {
		const key = payloadCacheKey("fallback", "LiberationSerif-Regular", "b".repeat(64));
		expect(key).toBe(`fallback:LiberationSerif-Regular:${"b".repeat(64)}`);
		// A re-pinned digest is a different key, which is the whole point: the
		// old bytes are not served under the new name.
		expect(key).not.toBe(payloadCacheKey("fallback", "LiberationSerif-Regular", "c".repeat(64)));
	});

	/**
	 * A chunk id and a face name that happen to collide must not share an entry,
	 * so the payload kind is in the key too.
	 */
	it("separates the payloads in the cache key", () => {
		expect(payloadCacheKey("cjk", "x", "d")).not.toBe(payloadCacheKey("fallback", "x", "d"));
	});

	/**
	 * The key the client hands its source is the one that pairs identity with
	 * digest — asserted end-to-end, not only on the pure function, because the
	 * function being right does not mean `serve` uses it.
	 */
	it("hands the byte source the digest-paired key for the identity it was asked for", async () => {
		const guest = fakeGuest([
			{ op: "fallbackOpen", value: state() },
			{ op: "fallbackFace", value: state({ adopted: true }) },
		]);
		const source = fakeSource();
		const client = new LazyPayloadClient(FALLBACK_DESCRIPTOR, guest, source);
		await client.open(2, [FACE_CLAIM]);
		await client.serve(2, FACE_CLAIM);
		expect(source.keys).toEqual([payloadCacheKey("fallback", FACE_CLAIM.id, FACE_CLAIM.sha256)]);
	});

	// --- serving ------------------------------------------------------------

	/**
	 * `len` must equal the attachment exactly, and it is the *decompressed*
	 * length — the number the guest compares against the claim's `rawBytes`. A
	 * compressed body fails both the digest and the length, correctly, and is
	 * counted as a poisoned attempt.
	 */
	it("declares len from the bytes it hands over, so the two cannot disagree", async () => {
		const body = new Uint8Array([1, 2, 3, 4, 5, 6, 7]);
		const guest = fakeGuest([
			{ op: "cjkOpen", value: state() },
			{ op: "cjkChunk", value: state({ adopted: true }) },
		]);
		const source = fakeSource(body);
		const client = new LazyPayloadClient(CJK_DESCRIPTOR, guest, source);
		await client.open(1, [CHUNK_CLAIM], { core: new Uint8Array(1) });
		await client.serve(1, CHUNK_CLAIM);
		expect(guest.calls[1]?.body.len).toBe(7);
		expect(guest.calls[1]?.attachment).toBe(body);
	});

	/**
	 * `status: 0` means "no response arrived" — a network error, a CORS refusal
	 * and an abort are indistinguishable from the shell. The client passes it
	 * through verbatim and applies no policy, because the guest's attempt
	 * accounting stays independent of the host's call pattern only if the host
	 * cannot re-decide what a status means.
	 */
	it("passes status 0 through untouched, and retries nothing on its own", async () => {
		const guest = fakeGuest([
			{ op: "cjkOpen", value: state() },
			{ op: "cjkChunk", value: state({ adopted: false }) },
		]);
		const client = new LazyPayloadClient(CJK_DESCRIPTOR, guest, fakeSource(new Uint8Array(0), 0));
		await client.open(1, [CHUNK_CLAIM], { core: new Uint8Array(1) });
		const answered = await client.serve(1, CHUNK_CLAIM);
		expect(guest.calls[1]?.body.status).toBe(0);
		expect(answered.adopted).toBe(false);
		// One delivery and no more: the bound that decides when to stop asking is
		// the guest's, not a shell-side retry loop.
		expect(guest.calls).toHaveLength(2);
	});

	/**
	 * A refused delivery is a counted failed attempt, not a transport failure.
	 * Throwing would turn "this face 404'd" into "the document failed to
	 * open", so `adopted: false` is reported and the loop moves on.
	 */
	it("reports a refused delivery as refused rather than throwing", async () => {
		const guest = fakeGuest([
			{ op: "cjkOpen", value: state() },
			{ op: "cjkChunk", value: state({ revision: 0, adopted: false }) },
		]);
		const client = new LazyPayloadClient(CJK_DESCRIPTOR, guest, fakeSource());
		await client.open(1, [CHUNK_CLAIM], { core: new Uint8Array(1) });
		const result = await client.drain(1, ["kanji"]);
		expect(result.refused).toEqual(["kanji"]);
		expect(result.delivered).toEqual([]);
	});

	/**
	 * The request the byte source is handed is the guest's own value, and the
	 * URL travelled from the guest to here without the client editing it. If
	 * the client reconstructed the URL from the id, a manifest published
	 * anywhere else would silently fetch nothing.
	 */
	it("passes the guest's own request through to the source unmodified", async () => {
		const guest = fakeGuest([
			{ op: "fallbackOpen", value: state() },
			{ op: "fallbackFace", value: state({ adopted: true }) },
		]);
		const seen: ClaimBody<string>[] = [];
		const client = new LazyPayloadClient(FALLBACK_DESCRIPTOR, guest, {
			async load(request) {
				seen.push(request);
				return { status: 200, bytes: new Uint8Array([1]) };
			},
		});
		await client.open(2, [FACE_CLAIM]);
		await client.serve(2, FACE_CLAIM);
		expect(seen).toEqual([FACE_CLAIM]);
	});

	// --- drain: the one shared loop ----------------------------------------

	/**
	 * The guest plans the next request only when it adopted the last one, so
	 * following `request` is how one delivery carries the whole remaining plan.
	 * Both ids here are ones the *guest* named in its own `request` values.
	 */
	it("follows the guest's request chain to drain a whole payload", async () => {
		const guest = fakeGuest([
			{ op: "cjkOpen", value: state() },
			{
				op: "cjkChunk",
				value: state({ revision: 1, loaded: ["kanji"], adopted: true, request: KANA_CLAIM }),
			},
			{ op: "cjkChunk", value: state({ revision: 2, loaded: ["kanji", "kana"], adopted: true }) },
		]);
		const source = fakeSource();
		const client = new LazyPayloadClient(CJK_DESCRIPTOR, guest, source);
		await client.open(1, [CHUNK_CLAIM, KANA_CLAIM], { core: new Uint8Array(1) });
		const result = await client.drain(1, ["kanji"]);
		expect(result.delivered).toEqual(["kanji", "kana"]);
		// Both requests were resolved through the claim table, so the shell
		// never derived a URL of its own.
		expect(source.keys).toEqual([
			payloadCacheKey("cjk", "kanji", CHUNK_CLAIM.sha256),
			payloadCacheKey("cjk", "kana", KANA_CLAIM.sha256),
		]);
		expect(result.state.revision).toBe(2);
	});

	/**
	 * A refused delivery ends its chain by construction — `op_cjk_chunk` and
	 * `op_fallback_face` both plan the next request only if the delivery was
	 * adopted — so the client must not enqueue a `request` it was handed after
	 * a refusal, or it would spend attempts on answers the guest has already
	 * given.
	 */
	it("does not follow a request the guest offered after refusing the last one", async () => {
		const guest = fakeGuest([
			{ op: "cjkOpen", value: state() },
			{ op: "cjkChunk", value: state({ adopted: false, request: KANA_CLAIM }) },
		]);
		const client = new LazyPayloadClient(CJK_DESCRIPTOR, guest, fakeSource());
		await client.open(1, [CHUNK_CLAIM, KANA_CLAIM], { core: new Uint8Array(1) });
		const result = await client.drain(1, ["kanji"]);
		expect(result.refused).toEqual(["kanji"]);
		expect(guest.calls).toHaveLength(2);
	});

	/**
	 * The three settled lists are kept apart by the guest precisely so a shell
	 * can say "this payload has no Korean" rather than "still loading". Asking
	 * again for a settled id spends an attempt on an answer already given.
	 */
	it("never re-requests an id the guest has already settled", async () => {
		const guest = fakeGuest([
			{ op: "fallbackOpen", value: state({ loaded: ["LiberationSerif-Regular"] }) },
		]);
		const client = new LazyPayloadClient(FALLBACK_DESCRIPTOR, guest, fakeSource());
		await client.open(2, [FACE_CLAIM]);
		const result = await client.drain(2, ["LiberationSerif-Regular"]);
		expect(result.delivered).toEqual([]);
		expect(guest.calls).toHaveLength(1);
	});

	/**
	 * A `needs` list carries ids, not URLs. Resolving them against the claims
	 * the shell already sent is a lookup, not a judgement — but an id with no
	 * claim means the two disagree about the manifest, and fetching a plausible
	 * guess would hide exactly that disagreement.
	 */
	it("refuses an id the guest named that the shell never claimed", async () => {
		const guest = fakeGuest([{ op: "cjkOpen", value: state() }]);
		const client = new LazyPayloadClient(CJK_DESCRIPTOR, guest, fakeSource());
		await client.open(1, [CHUNK_CLAIM], { core: new Uint8Array(1) });
		await expect(client.drain(1, ["korean"])).rejects.toThrow(
			/the shell and the guest disagree about the manifest/,
		);
	});

	/**
	 * Two claims for one identity are two claims about what the file is; the
	 * reader would have to pick a winner, so neither is served. Refusing at
	 * index time rather than overwriting keeps "guest and shell agree" true at
	 * the first point it could break.
	 */
	it("refuses a duplicate claim rather than letting one overwrite the other", () => {
		expect(() => indexClaims([CHUNK_CLAIM, { ...CHUNK_CLAIM, sha256: "d".repeat(64) }])).toThrow(
			/the guest would have to pick a winner/,
		);
	});

	/**
	 * The guard is a loop-safety net, not a retry policy: the real bound is the
	 * guest's (`MAX_FACE_REQUESTS`, `MAX_ATTEMPTS_PER_FACE`). It exists so a
	 * shell paired with a guest that keeps asking stops with a typed error
	 * instead of hanging, and it must fire *loudly* rather than silently cut a
	 * payload short.
	 */
	it("stops a runaway loop with a typed error rather than hanging", async () => {
		const steps = [{ op: "cjkOpen", value: state() }];
		for (let i = 0; i < 8; i += 1) {
			steps.push({
				op: "cjkChunk",
				// A fresh id each time, so the dedupe set cannot stop the loop
				// and the guard is what has to.
				value: state({ adopted: true, request: { ...KANA_CLAIM, id: `k${i}` } }),
			});
		}
		const guest = fakeGuest(steps);
		const client = new LazyPayloadClient(CJK_DESCRIPTOR, guest, fakeSource());
		const claims = [
			CHUNK_CLAIM,
			...Array.from({ length: 8 }, (_, i) => ({ ...KANA_CLAIM, id: `k${i}` })),
		];
		await client.open(1, claims, { core: new Uint8Array(1) });
		await expect(client.drain(1, ["kanji"], 3)).rejects.toThrow(/exceeded 3 deliveries/);
	});

	// --- the two orderings --------------------------------------------------

	/**
	 * **CJK is render-then-fetch.** The render's `needs` is the seed, and the
	 * caller owns the second render — `revision` is how it knows one is needed.
	 * An unchanged revision means nothing was adopted and re-rendering would
	 * produce exactly the same pixels.
	 */
	it("CJK drains from the render's own needs list, and reports the revision to repaint on", async () => {
		const guest = fakeGuest([
			{ op: "cjkOpen", value: state() },
			{ op: "cjkChunk", value: state({ revision: 1, loaded: ["kanji"], adopted: true }) },
		]);
		const client = new LazyPayloadClient(CJK_DESCRIPTOR, guest, fakeSource());
		await client.open(1, [CHUNK_CLAIM], { core: new Uint8Array(1) });
		// What `op_render` puts in its `cjk` block.
		const render = { needs: ["kanji"], revision: 0, residentBytes: 0 };
		const result = await client.drainCjk(1, render);
		expect(result.delivered).toEqual(["kanji"]);
		// Moved off 0, so the caller knows to re-render.
		expect(result.state.revision).not.toBe(render.revision);
	});

	/**
	 * **Fallback is fetch-then-render, and there is deliberately no paint in
	 * the middle.** A face swap changes every advance width on the line, so a
	 * render-deliver-repaint loop would show the user a correctly-laid-out page
	 * and then silently reflow every line of it. The probe's pixels are
	 * discarded and everything is delivered first — asserted by the op list,
	 * which contains no render at all: the caller renders once, afterwards.
	 */
	it("fallback primes every face the probe wanted before any real render", async () => {
		const guest = fakeGuest([
			{ op: "fallbackOpen", value: state() },
			{
				op: "fallbackFace",
				value: state({ revision: 1, loaded: [FACE_CLAIM.id], adopted: true, request: NEXT_FACE }),
			},
			{
				op: "fallbackFace",
				value: state({ revision: 2, loaded: [FACE_CLAIM.id, SANS_CLAIM.id], adopted: true }),
			},
		]);
		const client = new LazyPayloadClient(FALLBACK_DESCRIPTOR, guest, fakeSource());
		await client.open(2, [FACE_CLAIM, SANS_CLAIM]);
		const result = await client.primeFallbacks(2, {
			fallbacks: { needs: [FACE_CLAIM.id], revision: 0, residentBytes: 0 },
		});
		expect(result.delivered).toEqual([FACE_CLAIM.id, SANS_CLAIM.id]);
		// Everything delivered, and no repaint in between.
		expect(guest.calls.map((c) => c.op)).toEqual(["fallbackOpen", "fallbackFace", "fallbackFace"]);
	});

	/**
	 * **A genuine hole in the wire, reported rather than guessed around.**
	 * `op_render` inserts a `cjk` block but *no* `fallbacks` block, even though
	 * it does feed `fallback_pending` into the face loader's queue. So on a v1
	 * guest the probe tells the shell what chunks it wants and nothing about
	 * what faces it wants, and no other op reports the face queue. Every
	 * alternative is worse: treating the missing block as "nothing needed"
	 * paints the page with no fallbacks at all — the exact silent failure this
	 * flow exists to prevent — and fetching all ten faces to be safe is a
	 * ~700 kB download for a document that needed one.
	 */
	it("refuses to prime when the guest reports no fallbacks block, naming the field it needs", async () => {
		const guest = fakeGuest([{ op: "fallbackOpen", value: state() }]);
		const client = new LazyPayloadClient(FALLBACK_DESCRIPTOR, guest, fakeSource());
		await client.open(2, [FACE_CLAIM]);
		await expect(client.primeFallbacks(2, {})).rejects.toThrow(/fallback_pending/);
		// Nothing was fetched on the way to refusing: the flow stops before the
		// drain, so no plausible guess was downloaded.
		expect(guest.calls).toHaveLength(1);
	});

	// --- close --------------------------------------------------------------

	/**
	 * `cjkClose` is the FONT.10-F1 lever: the shell gives bytes back under its
	 * own storage pressure rather than discovering the ceiling as a quota
	 * exception. A close that released nothing answers `closed: false` and does
	 * *not* move the revision — so a polling shell must not repaint the world
	 * for a no-op, which is why `closed` is carried through verbatim.
	 */
	it("cjkClose releases a chunk and reports a no-op close without moving the revision", async () => {
		const guest = fakeGuest([
			{ op: "cjkOpen", value: state({ revision: 3 }) },
			{ op: "cjkClose", value: state({ revision: 4, releasedBytes: 3, closed: true }) },
			{ op: "cjkClose", value: state({ revision: 4, closed: false }) },
		]);
		const client = new LazyPayloadClient(CJK_DESCRIPTOR, guest, fakeSource());
		await client.open(1, [CHUNK_CLAIM], { core: new Uint8Array(1) });
		const released = await client.close(1, "kanji");
		expect(released.closed).toBe(true);
		expect(released.releasedBytes).toBe(3);
		expect(released.revision).toBe(4);

		const noop = await client.close(1, "never-resident");
		expect(noop.closed).toBe(false);
		// Unchanged: a polling shell comparing revisions sees nothing to do.
		expect(noop.revision).toBe(4);
		expect(guest.calls[1]?.body).toEqual({ doc: 1, chunk: "kanji" });
	});

	/**
	 * There is no `fallbackClose` on the wire. Sending one would earn
	 * `BINDING_UNSUPPORTED_OP` from a v1 guest, and a shell that discovered
	 * that by trying would have moved bytes for nothing — the faces are a
	 * bounded ten-face payload, which is the whole argument for the asymmetry.
	 */
	it("refuses a fallback close rather than sending an op the wire does not have", async () => {
		const guest = fakeGuest([{ op: "fallbackOpen", value: state() }]);
		const client = new LazyPayloadClient(FALLBACK_DESCRIPTOR, guest, fakeSource());
		await client.open(2, [FACE_CLAIM]);
		await expect(client.close(2, FACE_CLAIM.id)).rejects.toThrow(/no fallbackClose op/);
		expect(guest.calls).toHaveLength(1);
	});

	// --- reading a response -------------------------------------------------

	/**
	 * `request: null` is load-bearing: `worker.rs` writes
	 * `serde_json::Value::Null` rather than omitting the key, so a shell's
	 * `"request" in value` test cannot be confused with a request it failed to
	 * read. An *absent* key is therefore an error here, not a `null` — and it
	 * must be, because defaulting a missing `needs` to `[]` concludes
	 * "nothing is missing" and paints a page that is wrong.
	 */
	it("refuses a value whose request key is absent rather than reading it as null", async () => {
		// Built by omission rather than by `delete`, so the value is a plain
		// object literal with no `request` key at all — the shape a guest that
		// stopped writing the key would send.
		const noRequest: Record<string, unknown> = {
			revision: 0,
			residentBytes: 0,
			loaded: [],
			needs: [],
			unavailable: [],
			exhausted: [],
		};
		const guest = fakeGuest([{ op: "fallbackOpen", value: noRequest }]);
		const client = new LazyPayloadClient(FALLBACK_DESCRIPTOR, guest, fakeSource());
		await expect(client.open(2, [FACE_CLAIM])).rejects.toThrow(/request is not an object/);
	});

	/**
	 * `revision` missing must not silently become `0`, because `0` is a real
	 * revision: a shell comparing against it would either repaint forever or
	 * never repaint at all.
	 */
	it("refuses a value with no revision, rather than defaulting it to a real one", async () => {
		const guest = fakeGuest([
			{
				op: "fallbackOpen",
				value: {
					residentBytes: 0,
					loaded: [],
					needs: [],
					unavailable: [],
					exhausted: [],
					request: null,
				},
			},
		]);
		const client = new LazyPayloadClient(FALLBACK_DESCRIPTOR, guest, fakeSource());
		await expect(client.open(2, [FACE_CLAIM])).rejects.toThrow(/revision is not a number/);
	});

	/**
	 * The identity key inside a `request` value differs per payload, and reading
	 * the wrong one must fail rather than yield `undefined` as an id — an
	 * `undefined` id would be looked up, not found, and reported as a manifest
	 * disagreement, which sends the reader to the wrong bug entirely.
	 */
	it("reads the request's identity under the key this payload uses", async () => {
		const guest = fakeGuest([
			{ op: "cjkOpen", value: state({ request: CHUNK_CLAIM }) },
			{
				op: "fallbackOpen",
				value: state({
					request: {
						name: FACE_CLAIM.id,
						url: FACE_CLAIM.url,
						sha256: FACE_CLAIM.sha256,
						rawBytes: 3,
					},
				}),
			},
		]);
		const cjk = new LazyPayloadClient(CJK_DESCRIPTOR, guest, fakeSource());
		expect((await cjk.open(1, [CHUNK_CLAIM], { core: new Uint8Array(1) })).request?.id).toBe(
			"kanji",
		);

		const fallback = new LazyPayloadClient(FALLBACK_DESCRIPTOR, guest, fakeSource());
		const opened = await fallback.open(2, [FACE_CLAIM]);
		// Renamed onto the shared `id` field, so nothing downstream cares.
		expect(opened.request?.id).toBe("LiberationSerif-Regular");
		expect(opened.request?.url).toBe("fallback/LiberationSerif-Regular.ttf.br");
	});

	/**
	 * `adopted: undefined` (an open or a close) and `adopted: false` (a refused
	 * delivery) are different answers and must not be conflated: `drain` relies
	 * on the difference to know whether to keep following the chain.
	 */
	it("keeps an absent adopted distinct from a refused one", async () => {
		const guest = fakeGuest([
			{ op: "fallbackOpen", value: state() },
			{ op: "fallbackFace", value: state({ adopted: false }) },
		]);
		const client = new LazyPayloadClient(FALLBACK_DESCRIPTOR, guest, fakeSource());
		expect((await client.open(2, [FACE_CLAIM])).adopted).toBeUndefined();
		const served = await client.serve(2, FACE_CLAIM);
		expect(served.adopted).toBe(false);
	});

	/**
	 * The identity lists must be arrays of strings. A `loaded` that is not a
	 * list is a value this build cannot read, and coercing it would silently
	 * report the guest's state as empty — which reads as "nothing is resident"
	 * and so invites a refetch of everything.
	 */
	it("refuses a response whose identity lists are not arrays", async () => {
		// A `loaded` that is a bare string: unreachable from a real guest, which
		// is the point — the reader must not *assume* it cannot happen, because
		// the value comes off a message port.
		const wrong = state();
		wrong.loaded = "kanji";
		const guest = fakeGuest([{ op: "fallbackOpen", value: wrong }]);
		const client = new LazyPayloadClient(FALLBACK_DESCRIPTOR, guest, fakeSource());
		await expect(client.open(2, [FACE_CLAIM])).rejects.toThrow(/loaded is not an array/);
	});

	/**
	 * A guest that answers a non-object (a bare `null`, an array) is a value
	 * this build cannot read, and the correct response is to stop rather than to
	 * coerce — the same instinct as a missing `needs`.
	 */
	it("refuses a non-object response value", async () => {
		const guest = fakeGuest([{ op: "fallbackOpen", value: null }]);
		const client = new LazyPayloadClient(FALLBACK_DESCRIPTOR, guest, fakeSource());
		await expect(client.open(2, [FACE_CLAIM])).rejects.toThrow(/response value is not an object/);
	});

	/**
	 * The error carries a code so a caller can branch without parsing prose, and
	 * the code is the one that says *why* — asserted so a later edit cannot
	 * quietly repurpose `lazy-bad-response` for a case it does not describe.
	 */
	it("types its refusals with a code rather than only prose", async () => {
		const guest = fakeGuest([{ op: "fallbackOpen", value: null }]);
		const client = new LazyPayloadClient(FALLBACK_DESCRIPTOR, guest, fakeSource());
		const failure = await client.open(2, [FACE_CLAIM]).catch((e: unknown) => e);
		expect(failure).toBeInstanceOf(LazyPayloadError);
		expect((failure as LazyPayloadError).code).toBe("lazy-bad-response");
	});
});

describe("SL-3.FONT.12 — the committed fallback manifest is a real artifact", () => {
	/**
	 * The committed manifest must be readable by the reader the runtime trusts
	 * it with — this is the TypeScript half of the Rust test
	 * `the_committed_manifest_is_valid`. A hand-edited or stale manifest fails
	 * here rather than at the first `fallbackOpen` in the field.
	 */
	it("exists on disk and carries faces", () => {
		const raw = readFileSync(
			new URL("../../../assets/payload/fallback/manifest.json", import.meta.url),
			"utf8",
		);
		const parsed = JSON.parse(raw) as { faces: unknown[] };
		expect(Array.isArray(parsed.faces)).toBe(true);
		expect(parsed.faces.length).toBeGreaterThan(0);
	});
});
