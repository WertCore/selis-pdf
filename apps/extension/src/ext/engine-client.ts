/**
 * SL-4.EXT.06 — the viewer page's `EnginePort`, over the offscreen link.
 *
 * This is the file that makes "reuse `apps/ui`" true rather than aspirational:
 * `apps/ui` calls `adapter.engine.renderTile(...)` as an in-process interface,
 * and this module answers it across a JSON port. Everything `apps/ui` needs —
 * `DocHandle`, `RenderedTile`, `PageText`, progressive `SearchBatch`, prompt
 * cancellation, registry-coded rejections — is reconstructed here.
 *
 * ## Three things this transport cannot do, stated rather than hidden
 *
 * 1. **No intermediate progress.** A `Worker` port can push progress; a
 *    request/reply port cannot without becoming a stream. So `onProgress` fires
 *    at the two ends of each call with a coarse stage, and the UI must not treat
 *    it as a smooth bar. This is a property of the arrangement, not an
 *    oversight — `REUSE.md` says what it costs.
 * 2. **No transfer lists.** Tiles come back as base64 (see `engine-protocol.ts`),
 *    so the surface they reach has to be a main-thread one.
 * 3. **Only inline sources.** `Blob`, `File` and `FileSystemFileHandle` are not
 *    JSON, so the page reads the bytes and sends them.
 */

import type { EnginePort } from "../../../ui/src/platform/adapter.js";
import { AdapterError } from "../../../ui/src/platform/errors.js";
import type {
	AdapterRequestOptions,
	DocHandle,
	DocumentSourceDescriptor,
	PageText,
	RenderTileRequest,
	RenderedTile,
	SearchBatch,
	SearchOptions,
} from "../../../ui/src/platform/types.js";
import type { EngineLink } from "./engine-link.js";
import {
	type EngineRequest,
	type WireDoc,
	type WirePageText,
	type WireSearchBatch,
	type WireTile,
	base64ErrorReason,
	decodeBase64,
	encodeBase64,
} from "./engine-protocol.js";

/** Budget surface the viewer renders into; matches the Rust `SurfaceName`. */
export const VIEWER_SURFACE = "viewer";

/** Progress stages this transport can honestly report. */
const STAGE_OPEN = "open";
const STAGE_RENDER = "render";
const STAGE_TEXT = "text";
const STAGE_SEARCH = "search";

/**
 * Whether the caller's signal has fired.
 *
 * A function rather than an inline `options?.signal?.aborted === true` because
 * the streaming loop re-reads the signal after an `await`, and `tsc` narrows a
 * property access it has already seen as permanently `false`. Reading it through
 * a call is what keeps that narrowing from being wrong.
 */
function isAborted(options: AdapterRequestOptions | undefined): boolean {
	return options?.signal?.aborted === true;
}

/** One in-flight request. */
interface Pending {
	/** Settle a request/reply call. */
	resolve?: (value: unknown) => void;
	reject?: (error: AdapterError) => void;
	/** Stream calls: values are pushed here instead. */
	queue?: unknown[];
	/** Stream calls: a reader is parked on this. */
	wake?: (() => void) | null;
	/** Stream calls: the host said `done`, or we gave up. */
	finished: boolean;
}

/**
 * Read a supported descriptor's bytes.
 *
 * ## The refusals, and why each one is a refusal rather than a gap
 *
 * The adapter supports `bytes` and `blob` and refuses the rest:
 *
 * - `opfs` and `fsa` name a *place* the engine would have to resolve. In an
 *   extension page that place is the extension's own origin-private storage,
 *   which no PDF the user opened can legitimately name. Refusing keeps the
 *   engine's source vocabulary to "bytes a user handed us".
 * - `file` is a native path, which implies a host with filesystem access. The
 *   extension has none, and `file://` access is a user toggle that is
 *   SL-4.EXT.09's scope, not this task's.
 * - `http-range` would mean the engine re-fetching the document URL from inside
 *   the offscreen document. With `host_permissions: []` the extension has access
 *   to no document origin at all, so a range source is unreachable by
 *   construction. The intercepted-URL path does its one fetch in the page
 *   (EXT.02, `fetchInterceptedDocument`) and arrives here as `bytes`.
 *
 * All four reject with `BINDING_BAD_ARGUMENT` (6001), the code the mock already
 * uses for a capability the host does not expose: the argument failed
 * validation before entering the engine, and the document is untouched.
 */
export async function readSourceBytes(
	source: DocumentSourceDescriptor,
): Promise<{ name: string; bytes: Uint8Array }> {
	switch (source.kind) {
		case "bytes":
			return { name: source.name, bytes: new Uint8Array(source.bytes) };
		case "blob":
			return { name: source.name, bytes: new Uint8Array(await source.blob.arrayBuffer()) };
		default:
			throw AdapterError.badArgument(
				`the extension cannot open a '${source.kind}' document source - open a file, or open a document URL so the viewer can fetch it once and pass the bytes`,
			);
	}
}

/** Build the engine transport over `link`. */
export function createEnginePort(link: EngineLink): EnginePort {
	const pending = new Map<number, Pending>();
	let nextId = 1;

	link.subscribe((reply) => {
		const entry = pending.get(reply.id);
		if (entry === undefined) {
			// A reply to something we already settled (a cancel that raced a
			// reply) or never asked for. Dropping it is correct: there is no
			// longer anyone who can be told.
			return;
		}
		if (!reply.ok) {
			pending.delete(reply.id);
			entry.reject?.(toAdapterError(reply.error?.code, reply.error?.message));
			return;
		}
		if (entry.queue !== undefined) {
			entry.queue.push(reply.value);
			if (isSearchBatch(reply.value) && reply.value.done) {
				entry.finished = true;
			}
			entry.wake?.();
			entry.wake = null;
			return;
		}
		pending.delete(reply.id);
		entry.resolve?.(reply.value);
	});

	/**
	 * A request/reply call.
	 *
	 * Cancellation is resolved **locally first**: the promise rejects with 4020
	 * immediately and the `cancel` message is advisory. That ordering is what
	 * makes cancellation prompt rather than dependent on the host noticing, and
	 * it is why the UI never hangs when the offscreen document is busy.
	 */
	function call(
		build: () => Record<string, unknown>,
		options: AdapterRequestOptions | undefined,
		stage: string,
	): Promise<unknown> {
		if (isAborted(options)) {
			return Promise.reject(AdapterError.cancelled());
		}
		return new Promise<unknown>((resolve, reject) => {
			const id = nextId++;
			const entry: Pending = { finished: false };
			pending.set(id, entry);

			const onAbort = (): void => {
				if (!pending.delete(id)) {
					return;
				}
				options?.signal?.removeEventListener("abort", onAbort);
				// The host may ignore this; the rejection is what the UI sees.
				link.post({ v: 1, op: "cancel", id });
				reject(AdapterError.cancelled());
			};
			options?.signal?.addEventListener("abort", onAbort, { once: true });

			entry.resolve = (value) => {
				options?.signal?.removeEventListener("abort", onAbort);
				resolve(value);
			};
			entry.reject = (error) => {
				options?.signal?.removeEventListener("abort", onAbort);
				reject(error);
			};

			options?.onProgress?.({ fraction: 0, stage });
			link.post({ ...build(), v: 1, id } as EngineRequest);
		}).finally(() => {
			options?.onProgress?.({ fraction: 1, stage });
		});
	}

	/**
	 * A streaming call: several replies share one id, the last carrying `done`.
	 * A reader parked on {@link Pending.wake} is woken by the next arrival, so
	 * the async iterator suspends rather than spinning.
	 */
	async function* stream(
		build: () => Record<string, unknown>,
		options: AdapterRequestOptions | undefined,
	): AsyncGenerator<unknown, void, void> {
		if (isAborted(options)) {
			throw AdapterError.cancelled();
		}
		const id = nextId++;
		const entry: Pending = { queue: [], wake: null, finished: false };
		pending.set(id, entry);

		const onAbort = (): void => {
			if (!pending.delete(id)) {
				return;
			}
			link.post({ v: 1, op: "cancel", id });
			entry.finished = true;
			entry.wake?.();
			entry.wake = null;
		};
		options?.signal?.addEventListener("abort", onAbort, { once: true });

		try {
			link.post({ ...build(), v: 1, id } as EngineRequest);
			for (;;) {
				if (isAborted(options)) {
					throw AdapterError.cancelled();
				}
				const next = entry.queue?.shift();
				if (next !== undefined) {
					options?.onProgress?.({
						fraction: isSearchBatch(next) ? next.progress : 0,
						stage: STAGE_SEARCH,
					});
					yield next;
					continue;
				}
				if (entry.finished || !pending.has(id)) {
					return;
				}
				await new Promise<void>((wake) => {
					entry.wake = wake;
				});
			}
		} finally {
			options?.signal?.removeEventListener("abort", onAbort);
			if (pending.delete(id) && !entry.finished) {
				link.post({ v: 1, op: "cancel", id });
			}
		}
	}

	return {
		async open(source, requestOptions): Promise<DocHandle> {
			const { name, bytes } = await readSourceBytes(source);
			const wire = await call(
				() => ({
					op: "open",
					src: { kind: "inline", name, bytes64: encodeBase64(bytes) },
					surface: VIEWER_SURFACE,
				}),
				requestOptions,
				STAGE_OPEN,
			);
			return toDocHandle(wire);
		},

		async close(doc, requestOptions): Promise<void> {
			await call(() => ({ op: "close", doc: doc.id }), requestOptions, "close");
		},

		async renderTile(request: RenderTileRequest, requestOptions): Promise<RenderedTile> {
			const wire = await call(
				() => ({
					op: "render",
					doc: request.doc.id,
					page: request.page,
					scale: request.scale,
					...(request.rect === undefined
						? {}
						: {
								rect: {
									x: request.rect.x,
									y: request.rect.y,
									width: request.rect.width,
									height: request.rect.height,
								},
							}),
				}),
				requestOptions,
				STAGE_RENDER,
			);
			return toRenderedTile(wire);
		},

		async extractText(doc, page, requestOptions): Promise<PageText> {
			const wire = await call(
				() => ({ op: "text", doc: doc.id, page }),
				requestOptions,
				STAGE_TEXT,
			);
			return toPageText(wire);
		},

		search(doc, query, searchOptions, requestOptions): AsyncIterable<SearchBatch> {
			const resolved: SearchOptions = searchOptions ?? {};
			const batches = stream(
				() => ({
					op: "search",
					doc: doc.id,
					query,
					caseSensitive: resolved.caseSensitive === true,
					wholeWord: resolved.wholeWord === true,
					fromPage: resolved.fromPage ?? 0,
				}),
				requestOptions,
			);
			return {
				async *[Symbol.asyncIterator]() {
					for await (const batch of batches) {
						yield toSearchBatch(batch);
					}
				},
			};
		},
	};
}

// ─────────────────────────────────────────────────────────────────────────────
// Wire → UI. Every function here narrows an `unknown` that arrived off a port.
// ─────────────────────────────────────────────────────────────────────────────

/** Turn a host-reported registry code into the typed error the UI expects. */
function toAdapterError(code: number | undefined, message: string | undefined): AdapterError {
	if (code === undefined) {
		return new AdapterError({
			code: 6000,
			message: message ?? "the engine host reported a failure with no code",
			docState: "Unchanged",
			retryable: false,
		});
	}
	return new AdapterError({
		code,
		message: message ?? `the engine host reported ${code}`,
		docState: "Loaded",
		retryable: false,
	});
}

function toDocHandle(wire: unknown): DocHandle {
	const doc = wire as WireDoc;
	if (
		typeof doc?.doc !== "string" ||
		typeof doc.pageCount !== "number" ||
		!Array.isArray(doc.pageSizes)
	) {
		throw AdapterError.badArgument("the engine host returned an unreadable document handle");
	}
	return { id: doc.doc, pageCount: doc.pageCount, pageSizes: doc.pageSizes };
}

function toRenderedTile(wire: unknown): RenderedTile {
	const tile = wire as WireTile;
	if (
		typeof tile?.bytes64 !== "string" ||
		typeof tile.width !== "number" ||
		typeof tile.height !== "number" ||
		tile.format !== "rgba8"
	) {
		throw AdapterError.badArgument("the engine host returned an unreadable tile");
	}
	let data: Uint8Array;
	try {
		data = decodeBase64(tile.bytes64);
	} catch (error) {
		throw AdapterError.badArgument(
			`the engine host returned a tile whose pixels are not decodable (${base64ErrorReason(error)})`,
		);
	}
	if (data.byteLength !== tile.width * tile.height * 4) {
		throw AdapterError.badArgument(
			`the engine host returned a ${tile.width}x${tile.height} tile carrying ${data.byteLength} bytes`,
		);
	}
	// The UI owns this buffer and may keep it; the port's copy is not aliased.
	const copy = new ArrayBuffer(data.byteLength);
	new Uint8Array(copy).set(data);
	return { page: tile.page, width: tile.width, height: tile.height, format: "rgba8", data: copy };
}

function toPageText(wire: unknown): PageText {
	const page = wire as WirePageText;
	if (typeof page?.text !== "string" || typeof page.page !== "number") {
		throw AdapterError.badArgument("the engine host returned unreadable page text");
	}
	return { page: page.page, text: page.text };
}

function isSearchBatch(value: unknown): value is WireSearchBatch {
	return typeof (value as WireSearchBatch)?.done === "boolean";
}

function toSearchBatch(wire: unknown): SearchBatch {
	const batch = wire as WireSearchBatch;
	if (!isSearchBatch(batch) || !Array.isArray(batch.matches)) {
		throw AdapterError.badArgument("the engine host returned an unreadable search batch");
	}
	return {
		matches: batch.matches.map((match) => ({
			page: match.page,
			start: match.start,
			end: match.end,
		})),
		progress: batch.progress,
		done: batch.done,
	};
}
