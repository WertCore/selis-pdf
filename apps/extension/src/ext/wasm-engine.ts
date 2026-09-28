/**
 * SL-4.EXT.03 - the `EnginePort` the offscreen document serves, backed by the
 * WASM engine Worker.
 *
 * ## The shape of the thing
 *
 * `apps/ui` calls `adapter.engine.renderTile(...)` as an in-process interface.
 * `engine-client.ts` answers that across the JSON port to this process, and
 * this module answers *it* by talking the WASM.01 protocol to the Worker that
 * owns the guest. Three hops, three vocabularies, and each one is a place
 * where a field could be dropped - so the mapping here is explicit and
 * validated rather than cast, in the same spirit as the client's own reply
 * validation on the other side of the port.
 *
 * ## What this module is not responsible for
 *
 * It does not know that the port is JSON, that the host keeps a document
 * table, or that the service worker can be killed. It is an `EnginePort` and
 * nothing else, which is what makes it testable in Node against a fake
 * channel (`wasm-engine.test.ts`).
 *
 * ## A gap in the registry, named rather than papered over
 *
 * The registry (`crates/selis-error/codes.toml`) has no code for "the engine
 * itself is not there": no host needed one until now, because a host with an
 * engine never had to say so. Inventing a number here is forbidden
 * (03-CONVENTIONS 3), so the closest registered id is used and the choice is
 * stated where it is made: `IO_READ_FAILED` (5000, `NotLoaded`) for an engine
 * whose bytes could not be read or compiled, which is literally what happened,
 * and `BINDING_BAD_ARGUMENT` (6001, `Unchanged`) for a reply that does not
 * obey the protocol it documents. A dedicated `BINDING_ENGINE_UNAVAILABLE` is
 * the honest fix and belongs in a registry change, not in a host.
 */

import type { EnginePort } from "../../../ui/src/platform/adapter.js";
import { AdapterError, ErrorCode } from "../../../ui/src/platform/errors.js";
import type {
	AdapterRequestOptions,
	BudgetProfile,
	DocHandle,
	DocumentSourceDescriptor,
	PageText,
	PageTextLayer,
	Rect,
	RenderTileRequest,
	RenderedTile,
	SearchBatch,
	SearchMatch,
	SearchOptions,
	Size,
	TextLayerChar,
	TextLayerLine,
} from "../../../ui/src/platform/types.js";
import type { GuestResponse } from "./wasm-guest.js";
import type { WorkerRequest, WorkerResponse } from "./wasm-worker.js";

/** The channel this engine needs: a Worker, or something shaped like one. */
export interface EngineChannel {
	post(message: unknown, transfer?: Transferable[]): void;
	onMessage(listener: (message: unknown) => void): () => void;
	/** A transport-level failure: the Worker died, or the module failed. */
	onError(listener: (error: Error) => void): () => void;
	terminate(): void;
}

/** Matches per batch the streaming search iterator emits. */
const SEARCH_BATCH_SIZE = 64;

/** One settled dispatch: the engine's response and its binary attachment. */
interface GuestReply {
	readonly response: GuestResponse;
	readonly attachment: Uint8Array;
}

/**
 * Narrow a message off the channel to a {@link WorkerResponse}.
 *
 * The channel delivers `unknown` - anything in this document can post on it -
 * so the shape is checked rather than assumed, and an unrecognised message is
 * dropped rather than interpreted. A guess here would be a guess about which
 * document a tile belongs to.
 */
function asWorkerResponse(message: unknown): WorkerResponse | null {
	if (typeof message !== "object" || message === null) {
		return null;
	}
	const kind = (message as { kind?: unknown }).kind;
	if (
		kind !== "ready" &&
		kind !== "init-error" &&
		kind !== "reply" &&
		kind !== "fault"
	) {
		return null;
	}
	return message as WorkerResponse;
}
/**
 * Build the engine over `channel`, loading `wasmUrl` on first use.
 *
 * Lazy on purpose: the offscreen document is created before a document is
 * opened, and a viewer that is closed without opening anything should not
 * have paid for a WASM compile. `start()` exposes the same work for a host
 * that would rather warm it while nobody is waiting.
 */
export function createWasmEngine(options: {
	channel: EngineChannel;
	wasmUrl: string;
}): EnginePort & { start(): Promise<void>; dispose(): void } {
	const { channel, wasmUrl } = options;
	/** Correlation ids. The guest echoes them, and they never repeat. */
	let nextId = 1;
	const pending = new Map<
		number,
		{ resolve: (reply: GuestReply) => void; reject: (error: Error) => void }
	>();
	/** The compile, started once. A rejected one is remembered, not retried. */
	let ready: Promise<void> | null = null;
	/** A failure that outlives the request that caused it. */
	let fatal: AdapterError | null = null;

	/** Refuse everything still in flight, because the channel is gone. */
	const failAll = (error: Error): void => {
		for (const entry of pending.values()) {
			entry.reject(error);
		}
		pending.clear();
	};

	channel.onMessage((message) => {
		const reply = asWorkerResponse(message);
		if (reply === null) {
			return;
		}
		if (reply.kind === "init-error") {
			// Recorded rather than thrown: `start()` is what the first request
			// awaits, and it turns this into a rejection carrying the message
			// the viewer can display.
			fatal = engineUnavailable(reply.message);
			return;
		}
		if (reply.kind === "fault") {
			const entry = pending.get(reply.id);
			pending.delete(reply.id);
			entry?.reject(
				new AdapterError({
					code: ErrorCode.BindingBadArgument,
					message: reply.message,
					docState: "Unchanged",
					retryable: false,
				}),
			);
			return;
		}
		if (reply.kind !== "reply") {
			return;
		}
		const entry = pending.get(reply.id);
		if (entry === undefined) {
			return;
		}
		pending.delete(reply.id);
		entry.resolve({
			response: reply.response as GuestResponse,
			attachment: new Uint8Array(reply.attachment ?? new ArrayBuffer(0)),
		});
	});

	channel.onError((error) => {
		// A Worker that died mid-render leaves nothing to cancel and no way to
		// retry: every caller waiting on it has to learn now, not hang.
		fatal = engineUnavailable(`the engine worker stopped: ${error.message}`);
		failAll(fatal);
	});

	/** Compile the guest, once, and report the same failure every time after. */
	const start = async (): Promise<void> => {
		if (ready === null) {
			ready = new Promise<void>((resolve, reject) => {
				const off = channel.onMessage((message) => {
					const reply = asWorkerResponse(message);
					if (reply?.kind === "ready") {
						off();
						resolve();
					} else if (reply?.kind === "init-error") {
						off();
						reject(engineUnavailable(reply.message));
					}
				});
				const init: WorkerRequest = { kind: "init", wasmUrl };
				channel.post(init);
			});
			// A rejected compile must not be retried by the next request: the
			// same bytes fail the same way, and a viewer reopening five
			// documents would compile five times and report five failures.
			void ready.catch(() => {});
		}
		await ready;
	};
	/**
	 * Send one WASM.01 request and wait for its answer.
	 *
	 * An aborted signal posts a `cancel` and rejects locally *at once*. The
	 * guest is the thing doing the work and only it can stop, so the message
	 * is what saves the work; but the caller is released immediately, because
	 * a UI that has already scrolled on should not be blocked by a render it
	 * has given up on. The guest's eventual answer is dropped on arrival.
	 */
	const call = async (
		body: Record<string, unknown>,
		attachment: Uint8Array | undefined,
		options: AdapterRequestOptions | undefined,
	): Promise<GuestReply> => {
		if (fatal !== null) {
			throw fatal;
		}
		await start();
		if (options?.signal?.aborted === true) {
			throw AdapterError.cancelled();
		}
		const id = nextId;
		nextId += 1;
		const promise = new Promise<GuestReply>((resolve, reject) => {
			pending.set(id, { resolve, reject });
		});
		const payload =
			attachment === undefined
				? undefined
				: (attachment.buffer.slice(
						attachment.byteOffset,
						attachment.byteOffset + attachment.byteLength,
					) as ArrayBuffer);
		const message: WorkerRequest = {
			kind: "request",
			id,
			request: { v: 1, id, ...body },
			...(payload === undefined ? {} : { payload }),
		};
		channel.post(message, payload === undefined ? undefined : [payload]);
		const onAbort = (): void => {
			const cancelId = nextId;
			nextId += 1;
			const cancel: WorkerRequest = {
				kind: "request",
				id: cancelId,
				request: { v: 1, id: cancelId, op: "cancel", target: id },
			};
			channel.post(cancel);
		};
		options?.signal?.addEventListener("abort", onAbort, { once: true });
		try {
			return await rejectOnAbort(promise, options?.signal);
		} finally {
			pending.delete(id);
			options?.signal?.removeEventListener("abort", onAbort);
		}
	};

	/** The reply's `value`, or the engine's own typed failure. */
	const valueOf = (reply: GuestReply): Record<string, unknown> => {
		const { response } = reply;
		if (response.ok !== true) {
			throw new AdapterError({
				code: response.code ?? ErrorCode.BindingBadArgument,
				message: response.message ?? "the engine failed without saying why",
				docState:
					(response.docState as AdapterError["docState"] | undefined) ?? "Unchanged",
				retryable: response.code === ErrorCode.Cancelled,
			});
		}
		if (typeof response.value !== "object" || response.value === null) {
			throw AdapterError.badArgument("the engine answered without a result object");
		}
		return response.value as Record<string, unknown>;
	};

	/**
	 * `open` hands the guest the document as the request's attachment.
	 *
	 * Inline `bytes` is the only source the extension can offer, and the
	 * reason is upstream of here: with `host_permissions: []` there is no
	 * document origin to range-request, and a `Blob`/`File`/OPFS handle is not
	 * something an engine in another context could reach. The page read the
	 * file and sent the bytes; this is where they stop being base64.
	 */
	async function open(
		source: DocumentSourceDescriptor,
		options?: AdapterRequestOptions & { budget?: BudgetProfile },
	): Promise<DocHandle> {
		if (source.kind !== "bytes") {
			throw AdapterError.badArgument(
				`the extension engine opens inline bytes, not a ${source.kind} source`,
			);
		}
		const bytes = new Uint8Array(source.bytes);
		const reply = await call(
			{
				op: "open",
				src: { kind: "bytes", len: bytes.byteLength },
				budget: { surface: "viewer", ...budgetOverrides(options?.budget) },
			},
			bytes,
			options,
		);
		options?.onProgress?.({ fraction: 1, stage: "open" });
		return toDocHandle(valueOf(reply));
	}

	async function close(doc: DocHandle, options?: AdapterRequestOptions): Promise<void> {
		await call({ op: "close", doc: rawHandle(doc) }, undefined, options);
	}
	/**
	 * One tile.
	 *
	 * `scale` is device pixels per PDF point, so it becomes the guest's `dpi`
	 * (72 dpi is one pixel per point). A `rect` is in **page space**, which is
	 * y-up from the MediaBox's bottom-left, while the guest's tile rectangle
	 * indexes a raster canvas top-down - so the flip is done here, once, with
	 * the page's own height. Getting this wrong does not fail loudly: it
	 * renders the right pixels from the wrong part of the page.
	 */
	async function renderTile(
		request: RenderTileRequest,
		options?: AdapterRequestOptions,
	): Promise<RenderedTile> {
		const scale = request.scale;
		const pageHeight = request.doc.pageSizes[request.page]?.height ?? 0;
		const tile =
			request.rect === undefined
				? undefined
				: {
						x: Math.round(request.rect.x * scale),
						y: Math.round((pageHeight - request.rect.y - request.rect.height) * scale),
						w: Math.round(request.rect.width * scale),
						h: Math.round(request.rect.height * scale),
					};
		const reply = await call(
			{
				op: "render",
				doc: rawHandle(request.doc),
				page: request.page,
				params: {
					dpi: 72 * scale,
					...(tile === undefined ? {} : { tile }),
				},
			},
			undefined,
			options,
		);
		options?.onProgress?.({ fraction: 1, stage: "render" });
		return toRenderedTile(valueOf(reply), request.page, reply.attachment);
	}

	async function extractText(
		doc: DocHandle,
		page: number,
		options?: AdapterRequestOptions,
	): Promise<PageText> {
		const reply = await call({ op: "text", doc: rawHandle(doc), page }, undefined, options);
		valueOf(reply);
		options?.onProgress?.({ fraction: 1, stage: "text" });
		// The text leaves as the response's attachment, not as a JSON field: a
		// page of extracted text is thousands of characters, and a string in
		// the message body would be a second encoding of the same bytes.
		return { page, text: new TextDecoder().decode(reply.attachment) };
	}

	/**
	 * Per-character geometry, in the shape `PageTextLayer` defines.
	 *
	 * The guest assembles the layer (`selis-pdf-text`'s `page_layer`) and this
	 * module refuses anything that breaks its rule: `chars` is index-aligned
	 * with the line's `text`. A shell that trusted the shape would reintroduce
	 * exactly the desynchronisation SL-4.UI.04 exists to prevent.
	 */
	async function textLayer(
		doc: DocHandle,
		page: number,
		options?: AdapterRequestOptions,
	): Promise<PageTextLayer> {
		const reply = await call({ op: "textLayer", doc: rawHandle(doc), page }, undefined, options);
		const layer = toPageTextLayer(valueOf(reply), page);
		options?.onProgress?.({ fraction: 1, stage: "text" });
		return layer;
	}

	/**
	 * Search, as the progressive iterator the contract asks for.
	 *
	 * The guest answers a search in a single response - one WASM call cannot
	 * be interleaved with anything - so the batches are cut here. That is a
	 * real difference from a threaded shell and it is stated in `REUSE.md`:
	 * what the UI sees is a quick sequence of slices, not a page-by-page scan
	 * of a 5 000-page report.
	 *
	 * `wholeWord` is refused rather than ignored. The WASM.01 search options
	 * have no word-boundary flag, and a shell that dropped the request would
	 * answer a different question than the UI asked - reporting it as
	 * unsupported is the only honest option until the protocol grows one.
	 */
	async function* search(
		doc: DocHandle,
		query: string,
		searchOptions?: SearchOptions,
		options?: AdapterRequestOptions,
	): AsyncIterable<SearchBatch> {
		if (searchOptions?.wholeWord === true) {
			throw AdapterError.badArgument(
				"whole-word search has no flag in the WASM.01 search options (SL-4.EXT.03)",
			);
		}
		options?.onProgress?.({ fraction: 0, stage: "search" });
		const reply = await call(
			{
				op: "search",
				doc: rawHandle(doc),
				query,
				opts: {
					...(searchOptions?.caseSensitive === true ? { matchCase: true } : {}),
					...(searchOptions?.fromPage === undefined
						? {}
						: { pages: { from: searchOptions.fromPage, to: Math.max(0, doc.pageCount - 1) } }),
				},
			},
			undefined,
			options,
		);
		const matches = toSearchMatches(valueOf(reply));
		for (let at = 0; at < matches.length; at += SEARCH_BATCH_SIZE) {
			if (options?.signal?.aborted === true) {
				return;
			}
			yield {
				matches: matches.slice(at, at + SEARCH_BATCH_SIZE),
				progress: at / matches.length,
				done: false,
			};
		}
		options?.onProgress?.({ fraction: 1, stage: "search" });
		yield { matches: [], progress: 1, done: true };
	}

	return {
		open,
		close,
		renderTile,
		extractText,
		textLayer,
		search,
		start,
		dispose() {
			failAll(
				engineUnavailable("the engine host is shutting down"),
			);
			channel.terminate();
		},
	};
}
/** The engine mints a numeric handle; the UI's is an opaque string. */
function rawHandle(doc: DocHandle): number {
	const raw = Number(doc.id);
	if (!Number.isInteger(raw) || raw <= 0) {
		// A handle this host never issued, arriving from the page. Refusing it
		// here is the same answer the guest would give, one hop earlier.
		throw AdapterError.badHandle(`'${doc.id}' is not a handle this engine issued`);
	}
	return raw;
}

/**
 * The budget, in the wire's own spelling.
 *
 * `ADR-P0006`: the caller chooses the limits and the engine never picks its
 * own, so an absent budget means the viewer surface's profile unchanged
 * rather than a default this module invented.
 */
function budgetOverrides(budget: BudgetProfile | undefined): Record<string, unknown> {
	if (budget === undefined) {
		return {};
	}
	return {
		overrides: {
			...(budget.bytes === undefined ? {} : { bytes: budget.bytes }),
			...(budget.wallMs === undefined ? {} : { wallMs: budget.wallMs }),
			...(budget.objects === undefined ? {} : { objects: budget.objects }),
			...(budget.pixels === undefined ? {} : { pixels: budget.pixels }),
		},
	};
}

/** An engine that is not there, as the closest registered code allows. */
function engineUnavailable(message: string): AdapterError {
	return new AdapterError({
		code: ErrorCode.IoReadFailed,
		message,
		docState: "NotLoaded",
		retryable: false,
	});
}

/** Reject as soon as the signal fires, whatever the guest is doing. */
function rejectOnAbort<T>(promise: Promise<T>, signal: AbortSignal | undefined): Promise<T> {
	if (signal === undefined) {
		return promise;
	}
	return new Promise<T>((resolve, reject) => {
		const onAbort = (): void => reject(AdapterError.cancelled());
		signal.addEventListener("abort", onAbort, { once: true });
		promise.then(resolve, reject).finally(() => {
			signal.removeEventListener("abort", onAbort);
		});
	});
}

function toDocHandle(value: Record<string, unknown>): DocHandle {
	const doc = value.doc;
	const pages = value.pages;
	const sizes = value.pageSizes;
	if (typeof doc !== "number" || typeof pages !== "number" || !Array.isArray(sizes)) {
		throw AdapterError.badArgument("the engine opened a document with no readable handle");
	}
	// One size per page, because the UI's `DocHandle` is index-aligned with
	// page numbers and lays a page out before rendering it. A short array is
	// padded rather than trimmed: a missing slot is a page nobody can render
	// yet, and dropping the tail would renumber every page after it.
	const pageSizes: Size[] = [];
	for (let page = 0; page < pages; page += 1) {
		const size = sizes[page] as { width?: unknown; height?: unknown } | undefined;
		pageSizes.push({
			width: typeof size?.width === "number" ? size.width : 0,
			height: typeof size?.height === "number" ? size.height : 0,
		});
	}
	return { id: String(doc), pageCount: pages, pageSizes };
}

function toRenderedTile(
	value: Record<string, unknown>,
	page: number,
	attachment: Uint8Array,
): RenderedTile {
	const width = value.width;
	const height = value.height;
	if (typeof width !== "number" || typeof height !== "number") {
		throw AdapterError.badArgument("the engine rendered a tile with no dimensions");
	}
	if (attachment.byteLength !== width * height * 4) {
		throw AdapterError.badArgument(
			`the engine rendered a ${width}x${height} tile carrying ${attachment.byteLength} bytes`,
		);
	}
	// A copy, not the attachment itself: the UI owns this buffer and may keep
	// it while the next tile is on its way.
	const data = new ArrayBuffer(attachment.byteLength);
	new Uint8Array(data).set(attachment);
	return { page, width, height, format: "rgba8", data };
}
/**
 * The engine's layer, as the UI's - validated, and with the alignment rule
 * enforced rather than assumed.
 *
 * Every field is checked. A rectangle missing a component is a highlight
 * drawn somewhere arbitrary that still type-checks, and a shell that cannot
 * tell a correct layer from a subtly wrong one is worse off than one that
 * refuses it.
 */
function toPageTextLayer(value: Record<string, unknown>, page: number): PageTextLayer {
	const width = value.width;
	const height = value.height;
	const text = value.text;
	const lines = value.lines;
	if (
		typeof width !== "number" ||
		typeof height !== "number" ||
		typeof text !== "string" ||
		!Array.isArray(lines)
	) {
		throw AdapterError.badArgument("the engine answered with an unreadable text layer");
	}
	return {
		page,
		width,
		height,
		text,
		lowConfidence: value.lowConfidence === true,
		lines: lines.map(toTextLayerLine),
	};
}

function toTextLayerLine(line: unknown): TextLayerLine {
	const candidate = line as {
		text?: unknown;
		rect?: unknown;
		direction?: unknown;
		chars?: unknown;
	};
	if (
		typeof candidate?.text !== "string" ||
		!Array.isArray(candidate.chars) ||
		(candidate.direction !== "ltr" && candidate.direction !== "rtl")
	) {
		throw AdapterError.badArgument("the engine answered with an unreadable text line");
	}
	const chars = candidate.chars.map(toTextLayerChar);
	// The engine's own invariant, re-checked here because this is the boundary
	// a JavaScript string's UTF-16 indexing depends on: `chars[i]` describes
	// `text[i]`. A layer one entry short would select the wrong characters
	// and copy the wrong string, with no error anywhere.
	if (chars.length !== candidate.text.length) {
		throw AdapterError.badArgument(
			`the engine sent ${chars.length} character quads for ${candidate.text.length} characters`,
		);
	}
	return {
		text: candidate.text,
		rect: toRect(candidate.rect),
		direction: candidate.direction,
		chars,
	};
}

function toTextLayerChar(char: unknown): TextLayerChar {
	const candidate = char as { rect?: unknown; advance?: unknown; inked?: unknown };
	if (typeof candidate?.advance !== "number") {
		throw AdapterError.badArgument("the engine answered with an unreadable text character");
	}
	return {
		rect: toRect(candidate.rect),
		advance: candidate.advance,
		// `false` is the only value that means anything here. The UI's rule is
		// that only an explicit false is uninked - a synthesised space or the
		// trailing half of an astral scalar.
		inked: candidate.inked !== false,
	};
}

function toRect(value: unknown): Rect {
	const rect = value as Rect;
	if (
		typeof rect?.x !== "number" ||
		typeof rect.y !== "number" ||
		typeof rect.width !== "number" ||
		typeof rect.height !== "number"
	) {
		throw AdapterError.badArgument("the engine answered with an unreadable rectangle");
	}
	return { x: rect.x, y: rect.y, width: rect.width, height: rect.height };
}

function toSearchMatches(value: Record<string, unknown>): SearchMatch[] {
	const matches = value.matches;
	if (!Array.isArray(matches)) {
		throw AdapterError.badArgument("the engine searched and answered with no matches array");
	}
	return matches.map((match) => {
		const candidate = match as { page?: unknown; start?: unknown; end?: unknown };
		if (
			typeof candidate?.page !== "number" ||
			typeof candidate.start !== "number" ||
			typeof candidate.end !== "number"
		) {
			throw AdapterError.badArgument("the engine answered with an unreadable search match");
		}
		return { page: candidate.page, start: candidate.start, end: candidate.end };
	});
}