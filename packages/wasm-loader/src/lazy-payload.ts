/**
 * The shared lazy-payload client (SL-4.WASM.07 for CJK, SL-3.FONT.12 for
 * fallback faces).
 *
 * ## What this module is
 *
 * One client over one transport, for two payloads the guest already built to be
 * the same shape. The guest's own words: `fallbackchunk.rs` calls itself "the
 * mirror of `cjkchunk`, deliberately", and the wire agrees — both loaders are
 * `open` a set of claims, `deliver` one file, and answer with the same
 * six-field state. The only things that differ are the *name* of the identity
 * field (`id` vs `name`), the *name* of the op that delivers it
 * (`cjkChunk`/`chunk` vs `fallbackFace`/`face`), and one extra field on the CJK
 * open (the core subset rides as that request's attachment). That is a
 * {@link PayloadDescriptor} and nothing more; there is no second client, no
 * second transport, and no branch in the delivery path.
 *
 * ## The one thing that is genuinely different: ordering
 *
 * This is the part worth reading twice, because getting it wrong is a
 * correctness bug and not a style one.
 *
 * **CJK is render-then-fetch.** The guest's render walk has no network handle.
 * It paints `.notdef` for any code the resident set cannot serve, and reports
 * what it wanted in `cjk.needs`. So the shell's loop is: render → read
 * `needs` → deliver → **re-render**. The intermediate frame is legitimately
 * wrong and is replaced, because a missing glyph is missing ink and nothing
 * moves when it appears.
 *
 * **Fallback faces are fetch-then-render.** A face is all-or-nothing where a
 * chunk is incremental: the missing chunk degrades one range, but a missing
 * face leaves a page's *text metrics* wrong. Substituting a font changes every
 * advance width on the line, so a "render, paint, deliver, repaint" loop would
 * reflow every line of the page after the user has already seen it laid out
 * wrong. The guest already reports the face a render wanted — `op_render` feeds
 * `fallback_pending` into the loader's queue — so the flow is: probe render →
 * read `needs` → deliver everything → **render for real, once.**
 *
 * Those two orderings cannot be one loop, and forcing them into one would make
 * one of them wrong. So they are two small explicit flows, {@link
 * LazyPayloadClient.drainCjk} and {@link LazyPayloadClient.primeFallbacks},
 * over the shared {@link LazyPayloadClient.drain} primitive. What the shared
 * primitive guarantees — and it is the same guarantee for both — is that **the
 * shell only ever moves bytes for an identity the guest has already named.**
 * Neither flow can invent a request, retry on its own schedule, or decide that
 * a body was good.
 *
 * ## The three invariants this module is built around
 *
 * 1. **The guest names what it wants; the shell only moves bytes.** Every
 *    request this client honours comes from a `needs` list or a `request` value
 *    the guest produced. A {@link LazyByteSource} is handed a URL that arrived
 *    in a response, never one the client derived. The request bodies carry no
 *    method and no body at all, which is how "never upload the document"
 *    (ADR-P0016) holds across the wire and not only in the guest.
 * 2. **Verify before you hand over.** The guest re-checks the SHA-256 against
 *    its own claim and refuses a mismatch — but a shell cache keyed on identity
 *    alone cannot tell a poisoned entry from a good one, so the key this
 *    client hands its source is `payload:id:digest`
 *    ({@link payloadCacheKey}, ADR-P0043 §3).
 * 3. **Defer to the guest on every judgement call.** Which face is missing,
 *    whether a digest matches, when to stop retrying, whether a body parses —
 *    all of that is the guest's, and this module contains no digest
 *    computation, no retry counter and no status-code policy. A refused
 *    delivery is not an error here; it is the guest charging an attempt, and
 *    the guest decides when the attempts are spent.
 *
 * ## No platform globals
 *
 * There is no `fetch`, no `Worker`, no `crypto` and no `document` anywhere in
 * this file, by the same rule ADR-P0044 states for `apps/**` (and enforces
 * there by `platform-globals.test.ts`). Network and message-passing are the two
 * injected ports, {@link LazyByteSource} and {@link LazyTransport}. A package
 * that reaches for a global is a package that cannot be tested without one, and
 * the whole reason this client is worth a shared module is that it *is*
 * testable.
 */

/** Which payload a descriptor describes. */
export type LazyPayloadKind = "cjk" | "fallback";

/**
 * One payload file's manifest claim, as it crosses the wire.
 *
 * The same type is both what the shell *sends* at open and what the guest
 * *answers* in `request`, and that is not a coincidence to be tidied away later:
 * `cjk_request_value` and `fallback_request_value` in `worker.rs` serialise
 * exactly the fields the claim carried, because the guest is echoing the
 * shell's own claim back. So "the guest asked for this" and "the shell
 * promised this" are the same record, and one type says so.
 *
 * The identity is called `id` on the wire for CJK and `name` for fallback; the
 * descriptor renames it on the way in and on the way out, so nothing downstream
 * of {@link readRequest} has to care which payload it is holding.
 */
export interface ClaimBody<Id extends string> {
	/** The chunk id or face name, under whichever field the payload uses. */
	readonly id: Id;
	/** Lowercase hex SHA-256 of the bytes the guest will parse. */
	readonly sha256: string;
	/** The **decompressed** length in bytes; the guest checks it exactly. */
	readonly rawBytes: number;
	/** Where the shell should get the bytes. Comes from the guest's `request`. */
	readonly url: string;
}

/** An op body, without the `v`/`id` envelope the transport owns. */
export type LazyOpBody = Readonly<Record<string, unknown>>;

/**
 * The guest's loader state, identical in shape for both payloads.
 *
 * `revision` is the field a shell watches: **a change is the repaint signal**
 * (ADR-P0043 §3). The four lists are the guest's own vocabulary for the three
 * different reasons an identity will not become resident, and they are kept
 * apart on purpose — "still loading" (`needs`), "this payload has no Korean"
 * (`unavailable`) and "we gave up on this one" (`exhausted`) are three different
 * things to tell a user, and a shell that collapsed them into "missing" would
 * show a spinner forever.
 */
export interface LazyState<Id extends string> {
	/** The set's revision counter. */
	readonly revision: number;
	/** Raw bytes resident across every delivered file. */
	readonly residentBytes: number;
	/** Identities the guest has adopted. */
	readonly loaded: readonly Id[];
	/** Identities the guest has queued and will accept. */
	readonly needs: readonly Id[];
	/** Identities this payload ships no file for. */
	readonly unavailable: readonly Id[];
	/** Identities the guest has stopped asking for. */
	readonly exhausted: readonly Id[];
	/** The next file to deliver, or `null`. Absent on a render response. */
	readonly request: ClaimBody<Id> | null;
	/**
	 * Whether the guest adopted the delivery. Only on deliver responses.
	 *
	 * `undefined` means the op does not report it (open, close), which is
	 * different from `false` meaning "refused" and must not be conflated — see
	 * {@link LazyPayloadClient.drain}, which relies on the difference to know
	 * when to stop.
	 */
	readonly adopted?: boolean;
	/** Whether a close actually released something. Only on close responses. */
	readonly closed?: boolean;
	/** Raw bytes a close released. Only on a close that released. */
	readonly releasedBytes?: number;
}

/**
 * The message port: one op body in, one response `value` out.
 *
 * The transport owns the `v`/`id` envelope, the `postMessage` and the
 * attachment transfer, and it is also where a typed-error response becomes a
 * rejection. That last part is deliberate: the client never inspects the
 * registry's numeric error codes, because the codes are the guest's taxonomy
 * and re-deciding what a code means in TypeScript is how two implementations
 * of the same rule drift apart.
 */
export interface LazyTransport {
	/**
	 * Post one op body to the guest and resolve with its response `value`.
	 *
	 * @param op The op name, e.g. `fallbackOpen`.
	 * @param body The op body, minus the envelope.
	 * @param attachment The op's binary attachment, or `null` when it takes none.
	 * @throws Whatever the host's error mapping throws; never a parsed error here.
	 */
	call(op: string, body: LazyOpBody, attachment: Uint8Array | null): Promise<unknown>;
}

/**
 * The network port: bytes for one identity the guest named.
 *
 * **The bytes handed back must be the decompressed ones.** The files the
 * generator publishes are brotli-compressed (`<name>.ttf.br`), and the guest
 * verifies the digest of the *decompressed* font and SFNT-parses it — a
 * compressed body fails both checks, correctly, and the delivery is counted as
 * a poisoned attempt. Decompression is transport, which is exactly whose job
 * ADR-P0043 §3 makes it.
 */
export interface LazyByteSource<Id extends string> {
	/**
	 * Resolve the bytes for `request`.
	 *
	 * @param request The guest's own `request` value. Use `request.url` and
	 *   nothing else to locate the bytes; do not reconstruct a URL.
	 * @param key The cache key this client derived — see {@link payloadCacheKey}.
	 *   A source with a cache must key on this rather than on `request.id`, or a
	 *   re-pinned digest serves the old bytes under the new name.
	 */
	load(request: ClaimBody<Id>, key: string): Promise<PayloadDelivery>;
}

/**
 * What arrived for one request.
 *
 * `status` is passed through **verbatim**, including the `0` that means "no
 * response arrived" — a network error, a CORS refusal and an abort are
 * indistinguishable from here, exactly as `rangeChunk` and both payload loaders
 * document. The guest reads `0` and `>= 500` as "retry, and charge an attempt",
 * and anything else that is not 200/206 as a typed error; this client applies
 * no policy of its own and must not, or the attempt accounting in
 * `retry_or_give_up` stops being independent of the host's call pattern.
 */
export interface PayloadDelivery {
	/** The HTTP status, or `0` for "no response arrived". */
	readonly status: number;
	/** The decompressed bytes the guest will verify against its claim. */
	readonly bytes: Uint8Array;
}

/**
 * Everything an open needs beyond the claims themselves.
 *
 * CJK carries the core subset as the open request's binary attachment; the
 * fallback open carries no attachment at all, because the two faces the web
 * module embeds are already inside the wasm binary. The descriptor owns that
 * difference so {@link LazyPayloadClient.open} does not.
 */
export interface OpenExtras {
	/** Top-level request fields beyond `doc` and `claims`. */
	readonly fields: LazyOpBody;
	/** The open op's binary attachment, or `null` when it takes none. */
	readonly attachment: Uint8Array | null;
}

/** The document-scoped inputs to {@link LazyPayloadClient.open}. */
export interface OpenInput {
	/**
	 * Identities this payload ships **no file** for — `unserved` for CJK,
	 * `unavailable` for fallback. The guest never asks for these, and reports
	 * them back so a shell can say "this payload has no Korean" rather than
	 * "still loading".
	 *
	 * Sent only when non-empty, matching the Rust `skip_serializing_if`.
	 */
	readonly absent?: readonly string[];
	/** `cjkOpen` only: the core subset, carried as the open attachment. */
	readonly core?: Uint8Array;
	/** The caller's resource limits, passed through untouched. */
	readonly budget?: LazyOpBody;
}

/**
 * Everything that differs between the two payloads, in one small value.
 *
 * This is the whole of the "payload-agnostic" claim: four field names, two op
 * names, and a builder for the one request that is not shaped like the other.
 * There is no behavioural flag here on purpose — if a future difference cannot
 * be expressed as a name or a field, it is a difference in *ordering*, and
 * ordering is expressed by the two flows, not by a switch inside the transport.
 */
export interface PayloadDescriptor {
	/** Which payload this is, for the cache key and for error prose. */
	readonly kind: LazyPayloadKind;
	/** `cjkOpen` or `fallbackOpen`. */
	readonly openOp: string;
	/** `cjkChunk` or `fallbackFace`. */
	readonly deliverOp: string;
	/**
	 * The identity field inside a claim and inside a `request` value: `id` for
	 * CJK, `name` for fallback. The same name in both places, which is why one
	 * field is enough.
	 */
	readonly claimKey: "id" | "name";
	/**
	 * The identity field inside the *delivery* body: `chunk` for CJK, `face`
	 * for fallback. A third name, and the reason this descriptor exists rather
	 * than two near-identical clients.
	 */
	readonly deliverKey: "chunk" | "face";
	/** The open body's list of identities with no file: `unserved`/`unavailable`. */
	readonly absentKey: "unserved" | "unavailable";
	/** The open body's extra fields and optional attachment. */
	extras(input: OpenInput): OpenExtras;
}

/** CJK: a core subset rides along, and the absent list is `unserved`. */
export const CJK_DESCRIPTOR: PayloadDescriptor = {
	kind: "cjk",
	openOp: "cjkOpen",
	deliverOp: "cjkChunk",
	claimKey: "id",
	deliverKey: "chunk",
	absentKey: "unserved",
	extras: (input) => {
		// `coreLen` must match the attachment exactly or the guest answers
		// BINDING_BAD_ARGUMENT, so the length is taken from the bytes rather
		// than asked for — there is no way to state it that could disagree.
		const core = input.core;
		if (core === undefined) {
			throw new LazyPayloadError(
				"lazy-open-invalid",
				"cjkOpen needs the core subset as its binary attachment",
			);
		}
		return {
			fields: {
				...(input.absent && input.absent.length > 0 ? { unserved: [...input.absent] } : {}),
				coreLen: core.byteLength,
			},
			attachment: core,
		};
	},
};

/** Fallback: no attachment at all, and the absent list is `unavailable`. */
export const FALLBACK_DESCRIPTOR: PayloadDescriptor = {
	kind: "fallback",
	openOp: "fallbackOpen",
	deliverOp: "fallbackFace",
	claimKey: "name",
	deliverKey: "face",
	absentKey: "unavailable",
	extras: (input) => ({
		fields: input.absent && input.absent.length > 0 ? { unavailable: [...input.absent] } : {},
		attachment: null,
	}),
};

/**
 * Every way this client refuses, as a code rather than prose.
 *
 * Two of these exist to stop the client from doing something it must never do.
 * `lazy-bad-response` means the guest sent a value this build cannot read, and
 * the correct response is to stop rather than to coerce — a shell that defaults
 * a missing `needs` to `[]` concludes "nothing is missing" and paints a page
 * that is wrong. `lazy-fallback-unreported` is the same instinct made specific
 * to the fetch-then-render flow; see {@link LazyPayloadClient.primeFallbacks}.
 */
export type LazyPayloadErrorCode =
	/** A response `value` did not have the shape the protocol pins. */
	| "lazy-bad-response"
	/** An `open` was asked for something its payload requires and did not get. */
	| "lazy-open-invalid"
	/** A `needs` entry names a claim this client never sent. */
	| "lazy-unknown-identity"
	/** The guest did not report what a probe render wanted, so we cannot prime. */
	| "lazy-fallback-unreported"
	/** The delivery loop exceeded its guard; the guest's own bound should stop it. */
	| "lazy-runaway-loop"
	/** The op this payload needs does not exist on the wire. */
	| "lazy-unsupported-op";

/** A typed refusal, carrying a code so a caller can branch without parsing prose. */
export class LazyPayloadError extends Error {
	constructor(
		readonly code: LazyPayloadErrorCode,
		message: string,
	) {
		super(message);
		this.name = "LazyPayloadError";
	}
}

/**
 * What a render reported about the payload, as it rides back with the pixels.
 *
 * ## Why the CJK block is required and the fallback block is a refusal
 *
 * `op_render` inserts a `cjk` block when a CJK loader is attached, and today it
 * inserts **no** `fallbacks` block — even though it does feed
 * `fallback_pending` into the face loader's queue (`worker.rs`, `op_render`).
 * So on a v1 guest a probe render tells the shell what chunks it wants and tells
 * it nothing about what faces it wants, and there is no other op that reports
 * the face queue: `fallback_state` is returned only by `fallbackOpen` (queue
 * still empty) and `fallbackFace` (which needs a delivery to have happened
 * first). That is a genuine hole in the wire, and
 * {@link LazyPayloadClient.primeFallbacks} refuses loudly rather than guessing
 * around it.
 *
 * Both blocks are read the same way, which is the point: when the guest grows
 * the missing field, this is a one-line change in the guest and a one-line
 * change here, not a second code path.
 */
export interface LazyRenderOutcome<Id extends string> {
	/** `cjk.needs` / `fallbacks.needs`: identities the walk wanted. */
	readonly needs: readonly Id[];
	/** `cjk.revision` / `fallbacks.revision`: the set's counter after the walk. */
	readonly revision: number;
	/** `cjk.residentBytes` / `fallbacks.residentBytes`. */
	readonly residentBytes: number;
}

/**
 * A render response value, with at most one block per attached payload.
 *
 * Generic over the identity only so a client narrowed to a particular id can
 * read its own block; a shell talking to the guest over the wire has
 * `RenderValue<string>`, because the wire's identity lists are plain strings.
 */
export interface RenderValue<Id extends string = string> {
	readonly cjk?: LazyRenderOutcome<Id>;
	readonly fallbacks?: LazyRenderOutcome<Id>;
}

/**
 * The cache key a shell byte cache must use: `payload:id:digest`.
 *
 * **Pairing the identity with the digest is the whole point** (ADR-P0043 §3). A
 * cache keyed on the face name alone cannot distinguish a good entry from a
 * poisoned one that happens to sit under the same name, and the two are not
 * distinguishable by looking at them — a tampered TTF is still a TTF. The guest
 * re-verifies every delivery against its own claim, so a poisoned *guest* copy
 * is caught; a poisoned *shell* copy is not, because the shell is the thing
 * that would be handing it over. The digest goes in the key so the two copies
 * are distinguishable at the only place they can be.
 *
 * The payload kind is in the key too, so a chunk id and a face name that happen
 * to collide cannot share an entry.
 */
export function payloadCacheKey(kind: LazyPayloadKind, id: string, sha256: string): string {
	return `${kind}:${id}:${sha256}`;
}

/**
 * Claims by identity, for turning a `needs` list into requests.
 *
 * ## Why resolving a `needs` entry against the shell's own claims is not a
 * judgement call
 *
 * A render's `cjk.needs` is a list of **ids** — it carries no URL, no digest and
 * no length, because `cjk.needs` is `outcome.needs`, a `BTreeSet<&str>` of chunk
 * ids. The shell has to map those ids to something it can move bytes for, and
 * it does that with the claim table *it already sent*, and only for ids the
 * guest named. It never invents a URL, never widens the set, and never asks for
 * anything the guest did not list.
 *
 * That is still the guest naming what it wants: the guest chose the ids, and the
 * shell is doing what a transport does with an address, which is look it up. An
 * id that is not in the table is not looked up loosely — it is
 * `lazy-unknown-identity`, because a guest asking for something the shell never
 * claimed means the two disagree about the manifest, and fetching a plausible
 * guess would hide exactly that.
 */
export type ClaimIndex<Id extends string> = ReadonlyMap<Id, ClaimBody<Id>>;

/** Index claims by identity, rejecting a duplicate rather than overwriting it. */
export function indexClaims<Id extends string>(claims: readonly ClaimBody<Id>[]): ClaimIndex<Id> {
	const index = new Map<Id, ClaimBody<Id>>();
	for (const claim of claims) {
		if (index.has(claim.id)) {
			throw new LazyPayloadError(
				"lazy-unknown-identity",
				`two claims for '${claim.id}': the guest would have to pick a winner`,
			);
		}
		index.set(claim.id, claim);
	}
	return index;
}

/**
 * The client: open a payload, deliver what the guest names, and report what
 * changed.
 *
 * One instance per payload per document. It holds no bytes — the guest holds the
 * resident copy and the host's {@link LazyByteSource} holds the durable one —
 * only the claim index it needs to turn the guest's ids back into addresses.
 */
export class LazyPayloadClient<Id extends string> {
	readonly #descriptor: PayloadDescriptor;
	readonly #transport: LazyTransport;
	readonly #source: LazyByteSource<Id>;
	#claims: ClaimIndex<Id> = new Map();
	#last: LazyState<Id> | null = null;

	constructor(descriptor: PayloadDescriptor, transport: LazyTransport, source: LazyByteSource<Id>) {
		this.#descriptor = descriptor;
		this.#transport = transport;
		this.#source = source;
	}

	/** Which payload this client speaks for. */
	get kind(): LazyPayloadKind {
		return this.#descriptor.kind;
	}

	/** The guest's most recent state, or `null` before the first `open`. */
	get state(): LazyState<Id> | null {
		return this.#last;
	}

	/**
	 * Install the loader on `doc` and learn the first thing it wants.
	 *
	 * The response's `request` is the guest naming one file, and it is `null` on
	 * a freshly-opened payload because nothing has rendered yet and the queue is
	 * empty. A document that has needed nothing is not an error, and a shell
	 * that treated `null` as a failure would fail every open.
	 */
	async open(
		doc: number,
		claims: readonly ClaimBody<Id>[],
		input: OpenInput = {},
	): Promise<LazyState<Id>> {
		this.#claims = indexClaims(claims);
		const extras = this.#descriptor.extras(input);
		const value = await this.#transport.call(
			this.#descriptor.openOp,
			{
				doc,
				claims: claims.map((claim) => wireClaim(this.#descriptor.claimKey, claim)),
				...extras.fields,
				...(input.budget === undefined ? {} : { budget: input.budget }),
			},
			extras.attachment,
		);
		this.#last = readState<Id>(value, this.#descriptor.claimKey);
		return this.#last;
	}

	/**
	 * Deliver one file and hand back the guest's answer.
	 *
	 * This is the only method that moves bytes, and it moves exactly the ones
	 * the guest named. The URL that reaches the source travelled from the guest
	 * to here without this client editing it.
	 */
	async serve(doc: number, request: ClaimBody<Id>): Promise<LazyState<Id>> {
		const key = payloadCacheKey(this.#descriptor.kind, request.id, request.sha256);
		const delivery = await this.#source.load(request, key);
		const value = await this.#transport.call(
			this.#descriptor.deliverOp,
			{
				doc,
				[this.#descriptor.deliverKey]: request.id,
				// Verbatim, including the `0` that means "nothing arrived".
				status: delivery.status,
				// The *decompressed* length: this is the number the guest compares
				// against its claim's `rawBytes`.
				len: delivery.bytes.byteLength,
			},
			delivery.bytes,
		);
		this.#last = readState<Id>(value, this.#descriptor.claimKey);
		return this.#last;
	}

	/**
	 * Deliver everything the guest has already named, following its chain.
	 *
	 * The shared primitive, and the only loop in this module. Its correctness
	 * argument is short because it does very little:
	 *
	 * * It only ever works from a seed list the guest produced. A seed is an id
	 *   the guest named, resolved through the claim table; nothing is added.
	 * * It stops on the guest's own signal. `op_cjk_chunk` and `op_fallback_face`
	 *   both plan the next request **only if the delivery was adopted** — `let
	 *   next = if adopted { loader.plan()? } else { None }` — so a refused
	 *   delivery ends its chain by construction. That is the guest's attempt
	 *   accounting doing its job, and this loop is written so it cannot override
	 *   it: there is no retry here, and no place to add one without visibly
	 *   breaking the `MAX_ATTEMPTS_PER_*` bound.
	 * * It skips ids the guest has already settled — `loaded`, `unavailable`,
	 *   `exhausted` — because the guest will not accept them, and asking anyway
	 *   would spend an attempt on an answer it has already given.
	 *
	 * `maxDeliveries` is a loop-safety guard, **not** a retry policy. The real
	 * bound is the guest's (`MAX_FACE_REQUESTS`, `MAX_ATTEMPTS_PER_FACE`); this
	 * only exists so a shell paired with a guest that keeps asking is stopped by
	 * a typed error instead of hanging. It is generous enough not to fire in
	 * normal operation — the whole committed fallback payload is ten faces.
	 */
	async drain(doc: number, seed: readonly Id[], maxDeliveries = 64): Promise<DrainResult<Id>> {
		const delivered: Id[] = [];
		const refused: Id[] = [];
		const queue: Id[] = [];
		const queued = new Set<Id>();

		const enqueue = (id: Id): void => {
			if (queued.has(id) || this.#settled(id)) return;
			queued.add(id);
			queue.push(id);
		};
		for (const id of seed) enqueue(id);

		while (queue.length > 0) {
			if (delivered.length + refused.length >= maxDeliveries) {
				throw new LazyPayloadError(
					"lazy-runaway-loop",
					`${this.#descriptor.kind} delivery loop exceeded ${maxDeliveries} deliveries; the guest's own request bound should have stopped this first`,
				);
			}
			const id = queue.shift() as Id;
			const state = await this.serve(doc, this.#claimFor(id));
			if (state.adopted === true) {
				delivered.push(id);
				// The guest's own next request, followed only here. `op_cjk_chunk`
				// and `op_fallback_face` both plan the next request *only if* the
				// delivery was adopted, so a refused delivery ends the chain by
				// construction. Enqueueing a `request` that arrived alongside a
				// refusal would spend an attempt on an answer the guest has
				// already given — and would make this loop's call pattern a
				// second, silent retry policy.
				const next = state.request;
				if (next !== null) enqueue(next.id);
			} else {
				// Not an error. The guest counted a failed attempt, kept no
				// bytes, and answers `request: null` until a new render queues
				// something. Throwing here would turn "this face was not served"
				// into "the document failed to open".
				refused.push(id);
			}
		}

		return { state: this.#last as LazyState<Id>, delivered, refused };
	}

	/** Whether the guest has already settled this id, so asking again is waste. */
	#settled(id: Id): boolean {
		const state = this.#last;
		if (state === null) return false;
		return (
			state.loaded.includes(id) || state.unavailable.includes(id) || state.exhausted.includes(id)
		);
	}

	/** The shell's own claim for an id the guest named, or a typed refusal. */
	#claimFor(id: Id): ClaimBody<Id> {
		const claim = this.#claims.get(id);
		if (claim === undefined) {
			throw new LazyPayloadError(
				"lazy-unknown-identity",
				`the guest asked for '${id}' and no claim for it was ever sent; the shell and the guest disagree about the manifest`,
			);
		}
		return claim;
	}

	/**
	 * CJK: render, read what the render wanted, deliver, then render again.
	 *
	 * **Render-then-fetch, and the intermediate frame is allowed to be wrong.** A
	 * chunk that is not resident yet paints `.notdef` for the codes it covers,
	 * which is missing ink in the right place; when the chunk lands the revision
	 * moves and the shell re-renders. Nothing on the page moves, because a
	 * glyph's absence is not a glyph's wrong width.
	 *
	 * The caller owns the second render, and `revision` is how it knows one is
	 * needed: an unchanged revision means nothing was adopted, and re-rendering
	 * would produce the same pixels.
	 */
	async drainCjk(doc: number, render: LazyRenderOutcome<Id>): Promise<DrainResult<Id>> {
		return await this.drain(doc, render.needs);
	}

	/**
	 * Fallback: probe, deliver everything, and only then render for real.
	 *
	 * **Fetch-then-render, and there is deliberately no paint in the middle.**
	 * The reason is metrics. A face is all-or-nothing where a chunk is
	 * incremental: swap in a font the document did not ask for and every advance
	 * width on the line changes, so a render-deliver-repaint loop would show the
	 * user a correctly-laid-out page and then silently reflow every line of it.
	 * `fallbackchunk.rs` says this outright — "a missing face leaves a page's
	 * *text* wrong, so the shell fetches before rendering" — and
	 * `Session::render_page_fallbacks` blocks on the faces for the same reason.
	 *
	 * So the first render is a **probe**: it runs the walk, reports what it
	 * wanted, and its pixels are discarded. That costs one walk of glyph
	 * resolution and buys a page that is never shown in the wrong font.
	 *
	 * ## The probe must report, or this refuses
	 *
	 * `op_render` feeds `fallback_pending` into the face loader's queue but does
	 * **not** put it in the response, unlike the `cjk` block it inserts a few
	 * lines earlier. On a v1 guest the probe therefore returns nothing usable
	 * here, and every alternative is worse:
	 *
	 * * Treating the missing block as "nothing needed" paints the page with no
	 *   fallbacks at all — the exact silent failure this flow exists to prevent.
	 * * Fetching all ten faces "to be safe" is a ~700 kB download for a document
	 *   that needed one, and it is the guest's request bound, not the shell's
	 *   guess, that is supposed to decide how much to fetch.
	 * * Re-opening to read `plan()` would replace the loader and lose the queue
	 *   the probe just filled.
	 *
	 * So this throws `lazy-fallback-unreported` and says which guest field it
	 * needs. When the guest grows it, this becomes a one-line read and nothing
	 * else moves — the drain underneath is already correct.
	 */
	async primeFallbacks(doc: number, probe: RenderValue<Id>): Promise<DrainResult<Id>> {
		const reported = probe.fallbacks;
		if (reported === undefined) {
			throw new LazyPayloadError(
				"lazy-fallback-unreported",
				"the probe render reported no `fallbacks` block, so the faces a page needs cannot be known before rendering; refusing rather than painting with the wrong font (needs a guest that reports the render's fallback_pending)",
			);
		}
		// Everything the probe asked for, fetched before a single real pixel.
		return await this.drain(doc, reported.needs);
	}

	/**
	 * Release one resident file's bytes — the FONT.10-F1 lever.
	 *
	 * CJK only: `cjkClose` exists on the wire and there is no `fallbackClose`
	 * counterpart, so a fallback client refuses rather than sending an op a v1
	 * guest answers `BINDING_UNSUPPORTED_OP`. The faces are small and bounded,
	 * which is the whole argument for the asymmetry.
	 *
	 * A close that released nothing answers `closed: false` with the revision
	 * unchanged, and returns that faithfully — a polling shell must not repaint
	 * the world because an eviction turned out to be a no-op.
	 */
	async close(doc: number, id: Id): Promise<LazyState<Id>> {
		if (this.#descriptor.kind !== "cjk") {
			throw new LazyPayloadError(
				"lazy-unsupported-op",
				"there is no fallbackClose op; the wire has cjkClose only, and the ten fallback faces are a bounded payload",
			);
		}
		const value = await this.#transport.call(
			"cjkClose",
			{ doc, [CJK_DESCRIPTOR.deliverKey]: id },
			null,
		);
		this.#last = readState<Id>(value, this.#descriptor.claimKey);
		return this.#last;
	}
}

/**
 * What one drain did, for the caller to decide what to do next.
 *
 * `revision` and `residentBytes` are the guest's, not a tally kept here, so a
 * shell cannot drift from the set it is talking to. The caller compares
 * `revision` against the one it rendered with: unchanged means nothing was
 * adopted and the render already stands.
 */
export interface DrainResult<Id extends string> {
	/** The guest's state after the last delivery. */
	readonly state: LazyState<Id>;
	/** Identities the guest adopted, in delivery order. */
	readonly delivered: readonly Id[];
	/**
	 * Identities the guest refused, in delivery order.
	 *
	 * A refusal is the guest charging a failed attempt against its own bound,
	 * not a transport failure. It is reported rather than thrown so the caller
	 * can render anyway — a page missing one face is wrong, but a page that
	 * never renders because one face 404'd is worse.
	 */
	readonly refused: readonly Id[];
}

/** One claim, in the wire shape for the identity field this payload uses. */
export function wireClaim(key: "id" | "name", claim: ClaimBody<string>): LazyOpBody {
	return { [key]: claim.id, sha256: claim.sha256, rawBytes: claim.rawBytes, url: claim.url };
}

/**
 * Read a response `value` into a {@link LazyState}.
 *
 * Structural only, and that is a deliberate limit. This does not check that a
 * digest is well-formed, that a length is plausible, or that a list is one the
 * guest ought to have sent, because every one of those is a judgement the guest
 * has already made and re-doing it in TypeScript is how two readers of one
 * protocol start disagreeing (invariant 3). What it *does* check is that the
 * fields are there: a value missing `revision` must not silently become `0`,
 * because `0` is a real revision, and a shell comparing against it would either
 * repaint forever or never repaint at all.
 */
function readState<Id extends string>(value: unknown, key: "id" | "name"): LazyState<Id> {
	const record = object(value, "response value");
	return {
		revision: integer(record.revision, "revision"),
		residentBytes: integer(record.residentBytes, "residentBytes"),
		loaded: idList<Id>(record.loaded, "loaded"),
		needs: idList<Id>(record.needs, "needs"),
		unavailable: idList<Id>(record.unavailable, "unavailable"),
		exhausted: idList<Id>(record.exhausted, "exhausted"),
		request: readRequest(record.request, key),
		...(typeof record.adopted === "boolean" ? { adopted: record.adopted } : {}),
		...(typeof record.closed === "boolean" ? { closed: record.closed } : {}),
		...(typeof record.releasedBytes === "number" ? { releasedBytes: record.releasedBytes } : {}),
	};
}

/**
 * The guest's `request` value, or `null` for "nothing is wanted".
 *
 * `null` is the guest's own signal and is load-bearing: `worker.rs` writes
 * `serde_json::Value::Null` rather than omitting the key, specifically so a
 * shell's `"request" in value` test cannot be confused with a request it failed
 * to read. An absent key is therefore an error here, not a `null`.
 */
function readRequest<Id extends string>(value: unknown, key: "id" | "name"): ClaimBody<Id> | null {
	if (value === null) return null;
	const record = object(value, "request");
	return {
		id: text(record[key], `request.${key}`) as Id,
		sha256: text(record.sha256, "request.sha256"),
		rawBytes: integer(record.rawBytes, "request.rawBytes"),
		url: text(record.url, "request.url"),
	};
}

/** A non-null, non-array object, or a typed refusal. */
function object(value: unknown, what: string): Record<string, unknown> {
	if (typeof value !== "object" || value === null || Array.isArray(value)) {
		throw new LazyPayloadError("lazy-bad-response", `${what} is not an object`);
	}
	return value as Record<string, unknown>;
}

/** A finite number field, or a typed refusal. */
function integer(value: unknown, what: string): number {
	if (typeof value !== "number" || !Number.isFinite(value)) {
		throw new LazyPayloadError("lazy-bad-response", `${what} is not a number`);
	}
	return value;
}

/** A string field, or a typed refusal. */
function text(value: unknown, what: string): string {
	if (typeof value !== "string") {
		throw new LazyPayloadError("lazy-bad-response", `${what} is not a string`);
	}
	return value;
}

/**
 * One of the guest's four identity lists, or a typed refusal.
 *
 * The `as Id[]` is the one place the client's own type parameter is asserted
 * rather than narrowed: the wire's identity lists are `Vec<String>` in Rust and
 * therefore `string[]` in JSON, so a narrower `Id` is the *caller's* promise
 * about what its own payloads use, not something this reader can check. The
 * alternative — refusing a value whose list is not `Id[]` — would be a runtime
 * check on a type parameter, which TypeScript erases and which would therefore
 * be a check that silently does nothing.
 */
function idList<Id extends string>(value: unknown, what: string): readonly Id[] {
	if (!Array.isArray(value)) {
		throw new LazyPayloadError("lazy-bad-response", `${what} is not an array`);
	}
	return value.map((entry, index) => text(entry, `${what}[${index}]`) as Id);
}
