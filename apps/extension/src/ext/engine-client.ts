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
 *
 * ## The navigation questions (SL-4.UI.06), and why they share this factory
 *
 * The four navigation reads — the outline, the `/PageLabels`, the `/Dests`
 * name tree and a page's link annotations — are part of **this** object rather
 * than a second one, and the reason is the link's own behaviour:
 * `link.subscribe` delivers every reply to **every** subscriber, and the
 * correlation id is the only key that says which waiter a reply belongs to.
 *
 * A second factory with its own id counter would hand out ids that collide
 * with this one's, and a reply would be delivered to the wrong waiter — a
 * bookmark list answering a tile request, or a tile answering a bookmark list.
 * So there is one pending table, one id counter and one subscriber, and the
 * navigation methods are four more methods on it.
 *
 * The page is a **trust boundary** here, and it is treated as one: the reply
 * arrives as untrusted JSON over a port, so every field is validated before it
 * reaches the UI's types. A `Rect` with a missing component is a highlight
 * somewhere arbitrary that still type-checks, and a link whose action was
 * dropped is a link the reader can click with no destination.
 */

import type { EnginePort, NavigationPort } from "../../../ui/src/platform/adapter.js";
import { AdapterError, ErrorCode } from "../../../ui/src/platform/errors.js";
import type {
	AdapterRequestOptions,
	DocHandle,
	DocumentSourceDescriptor,
	LinkAction,
	LinkActionKind,
	LinkAnnotation,
	NamedDestination,
	OutlineNode,
	OutlineReport,
	PageLabelRange,
	PageText,
	PageTextLayer,
	PdfDestination,
	Rect,
	RenderTileRequest,
	RenderedTile,
	SearchBatch,
	SearchOptions,
} from "../../../ui/src/platform/types.js";
import {
	ACTION_KINDS,
	DESTINATION_KINDS,
	LABEL_STYLES,
} from "../../../ui/src/viewer/navigation-wire.js";
import type { EngineLink } from "./engine-link.js";
import {
	type EngineRequest,
	type WireDestinations,
	type WireDoc,
	type WireError,
	type WireLinkAction,
	type WireOutline,
	type WireOutlineNode,
	type WirePageLabels,
	type WirePageLinks,
	type WirePageText,
	type WirePageTextLayer,
	type WireSearchBatch,
	type WireTile,
	base64ErrorReason,
	decodeBase64,
	encodeBase64,
	outOfRangePage,
} from "./engine-protocol.js";

/** Budget surface the viewer renders into; matches the Rust `SurfaceName`. */
export const VIEWER_SURFACE = "viewer";

/** Progress stages this transport can honestly report. */
const STAGE_OPEN = "open";
const STAGE_RENDER = "render";
const STAGE_TEXT = "text";
const STAGE_SEARCH = "search";
const STAGE_NAVIGATION = "navigation";

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

/**
 * Build the engine transport over `link`.
 *
 * The returned object is both an {@link EnginePort} and a
 * {@link NavigationPort}: the four navigation reads share this object's
 * pending table and id counter, because `link.subscribe` delivers every reply
 * to every subscriber and the correlation id is the only key that says which
 * waiter a reply belongs to. A second factory with its own counter would hand
 * out colliding ids, and a reply would be delivered to the wrong waiter.
 */
export function createEnginePort(link: EngineLink): EnginePort & NavigationPort {
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
			entry.reject?.(toAdapterError(reply.error));
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
			// The page number is checked at the same three points as
			// `pageLinks`: here the page holds the handle, so an out-of-range
			// page is refused with the seam's 6001 rather than being sent to a
			// host that would answer the engine's 1301.
			if (isAborted(requestOptions)) {
				throw AdapterError.cancelled();
			}
			const reason = outOfRangePage(request.page, request.doc.pageCount);
			if (reason !== null) {
				throw AdapterError.badArgument(reason);
			}
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

		/**
		 * Character quads for the text layer (SL-4.UI.04).
		 *
		 * Same shape as every other engine op here: a message to the WASM
		 * worker in the offscreen document, which is the same engine the web
		 * build uses. Nothing is synthesised on this side — the contract in
		 * `adapter.ts` requires a transport that cannot supply quads to
		 * *reject* rather than invent them, because a text layer positioned by
		 * guesswork is a selection that highlights the wrong words, which is
		 * worse than no selection at all.
		 *
		 * Added when UI.04 made `EnginePort.textLayer` required, after EXT.06
		 * had already been developed against the previous shape. The worker
		 * side of the op lands with EXT.03; until then the call rejects at the
		 * transport rather than returning empty quads that would look like a
		 * page with no text.
		 */
		async textLayer(doc, page, requestOptions): Promise<PageTextLayer> {
			const wire = await call(
				() => ({ op: "textLayer", doc: doc.id, page }),
				requestOptions,
				STAGE_TEXT,
			);
			return toPageTextLayer(wire);
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

		/**
		 * The document's bookmark outline (SL-4.UI.06).
		 *
		 * The host's answer carries three facts — whether the document *has* an
		 * outline, whether the engine could read it in full, and whether the
		 * engine pruned items it could not read. All three cross into the UI's
		 * `OutlineReport`; dropping them here would make "this document has no
		 * outline" and "the outline exists but was truncated" the same empty
		 * list. A refusal (this host cannot read outlines at all) still arrives
		 * as a typed error rather than as an empty report.
		 */
		async outline(doc, requestOptions): Promise<OutlineReport> {
			const wire = await call(
				() => ({ op: "outline", doc: doc.id }),
				requestOptions,
				STAGE_NAVIGATION,
			);
			return toOutlineReport(wire);
		},

		/** The document's `/PageLabels` ranges, in document order. */
		async pageLabels(doc, requestOptions): Promise<readonly PageLabelRange[]> {
			const wire = await call(
				() => ({ op: "pageLabels", doc: doc.id }),
				requestOptions,
				STAGE_NAVIGATION,
			);
			return toPageLabelRanges(wire);
		},

		/** The document's `/Dests` name tree. */
		async destinations(doc, requestOptions): Promise<readonly NamedDestination[]> {
			const wire = await call(
				() => ({ op: "destinations", doc: doc.id }),
				requestOptions,
				STAGE_NAVIGATION,
			);
			return toNamedDestinations(wire);
		},

		/**
		 * One page's link annotations.
		 *
		 * The page is checked here as well as at the host and in the guest:
		 * this side holds the `DocHandle` the host gave it, so it can refuse
		 * an out-of-range page without a round trip. Every action class
		 * crosses, including the ones ADR-0020 disables — the transport
		 * reports what the document says and the viewer decides what happens.
		 */
		async pageLinks(doc, page, requestOptions): Promise<readonly LinkAnnotation[]> {
			// The cancellation first: a cancelled request is cancelled whatever
			// its arguments say, and a reader who has scrolled on should not be
			// told their page number was wrong.
			if (isAborted(requestOptions)) {
				throw AdapterError.cancelled();
			}
			const reason = outOfRangePage(page, doc.pageCount);
			if (reason !== null) {
				throw AdapterError.badArgument(reason);
			}
			const wire = await call(
				() => ({ op: "pageLinks", doc: doc.id, page }),
				requestOptions,
				STAGE_NAVIGATION,
			);
			return toLinkAnnotations(wire);
		},
	};
}

// ─────────────────────────────────────────────────────────────────────────────
// Wire → UI. Every function here narrows an `unknown` that arrived off a port.
// ─────────────────────────────────────────────────────────────────────────────

/**
 * Turn a host-reported failure into the typed error the UI expects.
 *
 * The host's `docState` is carried across verbatim rather than guessed at. It
 * is the answer to "what happened to the user's document?", and the host is the
 * only side that knows: a bad handle is `Unchanged` because the document was
 * never touched, while a failed render mid-session is `Loaded`. Substituting a
 * constant here would make every failure look like the same thing, which is
 * exactly the flattening `docState` exists to prevent.
 */
function toAdapterError(wire: WireError | undefined): AdapterError {
	if (wire === undefined) {
		return new AdapterError({
			code: ErrorCode.BindingBadHandle,
			message: "the engine host reported a failure with no error detail",
			docState: "Unchanged",
			retryable: false,
		});
	}
	return new AdapterError({
		code: wire.code,
		message: wire.message,
		docState: wire.docState,
		retryable: wire.retryable,
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

/**
 * Map the engine host's text layer onto {@link PageTextLayer}.
 *
 * Every field is validated rather than cast. A malformed quad would position a
 * selection highlight somewhere arbitrary while still type-checking, and a
 * text layer that is subtly wrong is worse than one that is refused — the
 * caller cannot tell a silent geometry error from correct geometry.
 */
function toPageTextLayer(wire: unknown): PageTextLayer {
	const layer = wire as WirePageTextLayer;
	if (
		typeof layer?.page !== "number" ||
		typeof layer.width !== "number" ||
		typeof layer.height !== "number" ||
		typeof layer.text !== "string" ||
		!Array.isArray(layer.lines)
	) {
		throw AdapterError.badArgument("the engine host returned an unreadable text layer");
	}
	return {
		page: layer.page,
		width: layer.width,
		height: layer.height,
		text: layer.text,
		lowConfidence: layer.lowConfidence === true,
		lines: layer.lines.map((line) => {
			if (
				typeof line?.text !== "string" ||
				!Array.isArray(line.chars) ||
				(line.direction !== "ltr" && line.direction !== "rtl")
			) {
				throw AdapterError.badArgument("the engine host returned an unreadable text line");
			}
			return {
				text: line.text,
				rect: toRect(line.rect),
				direction: line.direction,
				// Index-aligned with `text` (see TextLayerLine). A mismatch
				// would desynchronise selection from the copied string, so it
				// is refused rather than padded or truncated.
				chars: line.chars.map((char: WirePageTextLayer["lines"][number]["chars"][number]) => {
					if (typeof char?.advance !== "number") {
						throw AdapterError.badArgument("the engine host returned an unreadable text character");
					}
					return { rect: toRect(char.rect), advance: char.advance, inked: char.inked !== false };
				}),
			};
		}),
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
		throw AdapterError.badArgument("the engine host returned an unreadable rectangle");
	}
	return { x: rect.x, y: rect.y, width: rect.width, height: rect.height };
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

// ─────────────────────────────────────────────────────────────────────────────
// The navigation replies (SL-4.UI.06).
//
// The wire already speaks the viewer's vocabulary, so these are not
// *translations* — they are **validations**. Every field is checked, because
// the page cannot tell a correct bookmark from a plausible one, and a link
// whose action was quietly dropped is a link the reader can click with no
// destination.
// ─────────────────────────────────────────────────────────────────────────────

/**
 * The host's outline, as the viewer's outline report.
 *
 * The three qualifiers the host sends — `present`, `truncated`, `pruned` —
 * are validated and **carried**, not dropped: the UI's `OutlineReport` has
 * fields for all three, so a truncated walk can be told apart from a document
 * with no outline. A *refusal* still arrives as a typed error rather than as
 * an empty report, so "this document has no outline" and "this host cannot
 * read outlines" do not look alike.
 */
function toOutlineReport(wire: unknown): OutlineReport {
	const outline = wire as WireOutline;
	if (
		typeof outline?.present !== "boolean" ||
		typeof outline.truncated !== "boolean" ||
		typeof outline.pruned !== "boolean" ||
		!Array.isArray(outline.items)
	) {
		throw AdapterError.badArgument("the engine host returned an unreadable outline");
	}
	return {
		present: outline.present,
		truncated: outline.truncated,
		pruned: outline.pruned,
		items: outline.items.map(toOutlineNode),
	};
}

/** One bookmark, as the viewer names it. */
function toOutlineNode(node: unknown): OutlineNode {
	const item = node as WireOutlineNode;
	if (typeof item?.title !== "string") {
		throw AdapterError.badArgument("the engine host returned an unreadable bookmark");
	}
	const children = Array.isArray(item.children) ? item.children.map(toOutlineNode) : [];
	return {
		title: item.title,
		...(item.destination === undefined ? {} : { destination: toDestination(item.destination) }),
		...(item.namedDestination === undefined
			? {}
			: { namedDestination: requireString(item.namedDestination, "bookmark name") }),
		...(item.descendantCount === undefined
			? {}
			: { descendantCount: requireNumber(item.descendantCount, "bookmark count") }),
		...(children.length === 0 ? {} : { children }),
	};
}

/** A resolved destination: a page, and how to place it. */
function toDestination(wire: unknown): PdfDestination {
	const destination = wire as WireDestinations["destinations"][number]["destination"];
	if (typeof destination?.page !== "number") {
		throw AdapterError.badArgument("the engine host returned an unreadable destination");
	}
	const kind = destination.kind;
	if (kind !== undefined && !DESTINATION_KINDS.includes(kind)) {
		// A class outside the viewer's vocabulary. The guest maps an
		// unrecognised *engine* class to `xyz`; a class that is not in the
		// viewer's list at all means the host and the page disagree about the
		// protocol, which is a bug worth refusing rather than absorbing.
		throw AdapterError.badArgument(
			`the engine host returned a destination of unknown kind '${kind}'`,
		);
	}
	return {
		page: destination.page,
		...(kind === undefined ? {} : { kind }),
		...(destination.left === undefined
			? {}
			: { left: requireOptionalNumber(destination.left, "destination offset") }),
		...(destination.top === undefined
			? {}
			: { top: requireOptionalNumber(destination.top, "destination offset") }),
		...(destination.zoom === undefined
			? {}
			: { zoom: requireOptionalNumber(destination.zoom, "destination zoom") }),
	};
}

/** The host's `/PageLabels` ranges. */
function toPageLabelRanges(wire: unknown): readonly PageLabelRange[] {
	const labels = wire as WirePageLabels;
	if (!Array.isArray(labels?.ranges)) {
		throw AdapterError.badArgument("the engine host returned unreadable page labels");
	}
	return labels.ranges.map((range) => {
		if (typeof range?.firstPage !== "number") {
			throw AdapterError.badArgument("the engine host returned an unreadable page-label range");
		}
		const style = range.style;
		if (style !== undefined && !LABEL_STYLES.includes(style)) {
			throw AdapterError.badArgument(
				`the engine host returned a page-label range of unknown style '${style}'`,
			);
		}
		return {
			firstPage: range.firstPage,
			...(style === undefined ? {} : { style }),
			...(range.prefix === undefined
				? {}
				: { prefix: requireString(range.prefix, "label prefix") }),
			...(range.firstValue === undefined
				? {}
				: { firstValue: requireNumber(range.firstValue, "label first value") }),
		};
	});
}

/** The host's `/Dests` name tree. */
function toNamedDestinations(wire: unknown): readonly NamedDestination[] {
	const named = wire as WireDestinations;
	if (!Array.isArray(named?.destinations)) {
		throw AdapterError.badArgument("the engine host returned unreadable destinations");
	}
	return named.destinations.map((entry) => {
		if (typeof entry?.name !== "string") {
			throw AdapterError.badArgument("the engine host returned an unnamed destination");
		}
		return { name: entry.name, destination: toDestination(entry.destination) };
	});
}

/**
 * One page's link annotations.
 *
 * Every action class crosses, **including the ones ADR-0020 disables**. The
 * transport reports what the document says; the viewer decides what happens.
 * A transport that dropped `/Launch` here would make "is the viewer refusing
 * this, or does the engine not know about it?" unanswerable — and the
 * viewer's refusal is the thing under test.
 */
function toLinkAnnotations(wire: unknown): readonly LinkAnnotation[] {
	const page = wire as WirePageLinks;
	if (!Array.isArray(page?.links)) {
		throw AdapterError.badArgument("the engine host returned unreadable link annotations");
	}
	return page.links.map((link) => {
		if (typeof link?.id !== "string") {
			throw AdapterError.badArgument("the engine host returned an unidentified link annotation");
		}
		return {
			id: link.id,
			rect: toRect(link.rect),
			action: toLinkAction(link.action),
			...(link.contents === undefined
				? {}
				: { contents: requireString(link.contents, "link contents") }),
		};
	});
}

/** A link's action, verbatim. */
function toLinkAction(wire: unknown): LinkAction {
	const action = wire as WireLinkAction;
	if (typeof action?.kind !== "string") {
		throw AdapterError.badArgument("the engine host returned an unreadable link action");
	}
	if (!ACTION_KINDS.includes(action.kind as LinkActionKind)) {
		// The guest maps a class the *engine* does not model to `unknown`, and
		// the viewer has a decision for `unknown`: refuse it and say so. A
		// class outside the viewer's own vocabulary means the host and the
		// page disagree about the protocol.
		throw AdapterError.badArgument(
			`the engine host returned a link action of unknown kind '${action.kind}'`,
		);
	}
	return {
		kind: action.kind as LinkActionKind,
		...(action.uri === undefined ? {} : { uri: requireString(action.uri, "link URI") }),
		...(action.name === undefined ? {} : { name: requireString(action.name, "link name") }),
		...(action.destination === undefined ? {} : { destination: toDestination(action.destination) }),
	};
}

/** A field that must be a string. */
function requireString(value: unknown, what: string): string {
	if (typeof value !== "string") {
		throw AdapterError.badArgument(`the engine host returned an unreadable ${what}`);
	}
	return value;
}

/** A field that must be a number. */
function requireNumber(value: unknown, what: string): number {
	if (typeof value !== "number") {
		throw AdapterError.badArgument(`the engine host returned an unreadable ${what}`);
	}
	return value;
}

/**
 * A field that must be a number or an explicit `null`.
 *
 * `null` is meaningful here: a `/XYZ` destination whose author left `left`
 * unset crosses as `null`, and the viewer tells "no offset" from "offset
 * zero". Losing that distinction would move the page.
 */
function requireOptionalNumber(value: unknown, what: string): number | null {
	if (value === null) {
		return null;
	}
	if (typeof value !== "number") {
		throw AdapterError.badArgument(`the engine host returned an unreadable ${what}`);
	}
	return value;
}
