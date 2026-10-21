/**
 * SL-4.EXT.06 - the offscreen document's half of the engine transport.
 *
 * ## The MV3 arrangement this exists to serve
 *
 * The web build gives the engine a `Worker` it spawns itself, so the engine's
 * lifetime is the page's lifetime and a structured clone carries bytes for free.
 * MV3 has neither half of that:
 *
 * - A **service worker** is killed after ~30 s idle and has no DOM, so it cannot
 *   host the WASM engine - and must not be handed document bytes anyway
 *   (24-BINDINGS-SPEC §5).
 * - A **`Worker` spawned by the viewer page** dies with the page, which is the
 *   one lifetime the `offscreen` permission exists to avoid: a user switching
 *   tabs or opening a PDF in a new tab must not lose the engine.
 * - The **offscreen document** is a real DOM document that outlives any single
 *   page, and it is the only place an MV3 extension can run a long-lived engine.
 *
 * So: `viewer.html` <-> `chrome.runtime` port <-> `offscreen.html`. This module
 * is the right-hand side, and it is written against the *shared* `EnginePort`
 * interface rather than a bespoke engine API, so SL-4.EXT.03 supplies a real
 * WASM engine by passing one in and changes nothing here.
 *
 * ## Why the host keeps its own document table
 *
 * `EnginePort` hands out opaque `DocHandle`s and validates them on the way back
 * in. The wire carries only the handle's `id`, so the host has to remember the
 * rest of the handle from `open` - and that table is also where "unknown or
 * closed handle" becomes `BINDING_BAD_HANDLE` (6000) instead of whatever the
 * engine would say about a handle it never issued. It doubles as the cleanup
 * list: when the port drops, every document still open is closed, because the
 * page that owned them is gone and nothing else would ever ask.
 *
 * ## The navigation capability, and what its absence means (SL-4.UI.06)
 *
 * The host serves the four navigation questions — the outline, the
 * `/PageLabels`, the `/Dests` name tree and a page's link annotations — from
 * the capability it is given, not from the engine port itself.
 *
 * A host with **no** capability refuses those four with
 * `BINDING_UNSUPPORTED_OP` (6017), and that refusal is the whole point of
 * the code: "this host cannot read outlines" is a fact about the host, not a
 * malformed request, and `BINDING_BAD_ARGUMENT` (6001) would say the request
 * was wrong. A viewer that was never given the capability would otherwise look
 * exactly like one that was given it and failed — and would tell a reader
 * that their document has no bookmarks when the truth is that nobody could
 * look.
 *
 * The distinction survives all the way to the reader because the three states
 * are three states: a **refusal** (6017) is "this host cannot read outlines",
 * an **empty list** is "this document has none", and a **populated list** is
 * "here it is".
 */

import type { EnginePort } from "../../../ui/src/platform/adapter.js";
import { AdapterError, ErrorCode } from "../../../ui/src/platform/errors.js";
import type {
	DocHandle,
	DocumentSourceDescriptor,
	SearchBatch,
} from "../../../ui/src/platform/types.js";
import { type EngineLink, isEngineRequest } from "./engine-link.js";
import {
	type EngineReply,
	type EngineRequest,
	type NavigationCapability,
	type WireAck,
	type WireDoc,
	type WireError,
	type WireSearchBatch,
	base64ErrorReason,
	decodeBase64,
	encodeBase64,
	outOfRangePage,
} from "./engine-protocol.js";

/**
 * Serve the engine half of `link`. The returned function tears it down.
 *
 * `engine` may be `null`, and that is the state this package ships in: the WASM
 * engine is SL-4.EXT.03's, and until it is installed the host still answers —
 * with `BINDING_BAD_HANDLE` (6000) and a message saying so. Failing loudly per
 * request is the point. The alternative, not answering at all, is
 * indistinguishable from a hung engine: `chrome.runtime.connect` does not queue,
 * so the viewer's request would simply vanish and the page would spin forever.
 *
 * `navigation` may be `null` too, and that is a **capability** answer rather
 * than a broken one: the four navigation ops then refuse with
 * `BINDING_UNSUPPORTED_OP` (6017), which says "this host cannot read
 * outlines" rather than "your request was wrong" or "this document has none".
 */
export function serveEngineHost(options: {
	link: EngineLink;
	engine: EnginePort | null;
	/**
	 * The navigation capability this host serves, or `null` for a host that
	 * cannot answer the four navigation questions at all.
	 */
	navigation?: NavigationCapability | null;
}): () => void {
	const { link, engine, navigation = null } = options;
	const requireEngine = (): EnginePort => {
		if (engine === null) {
			throw new AdapterError({
				code: ErrorCode.BindingBadHandle,
				message:
					"the offscreen engine host is running but no engine is installed yet (SL-4.EXT.03)",
				docState: "NotLoaded",
				retryable: false,
			});
		}
		return engine;
	};
	/**
	 * The navigation capability, or a refusal that says what is missing.
	 *
	 * The check comes *after* the document handle in the callers, because the
	 * handle is the subject of the request: a request naming a document this
	 * host never issued is malformed whatever the host can do, and a request
	 * naming a live document is refused for the capability reason.
	 */
	const requireNavigation = (): NavigationCapability => {
		if (navigation === null) {
			throw new AdapterError({
				code: ErrorCode.BindingUnsupportedOp,
				message:
					"this engine host cannot read outlines, page labels, destinations or link annotations: no navigation capability is installed",
				docState: "Unchanged",
				retryable: false,
			});
		}
		return navigation;
	};
	const documents = new Map<string, DocHandle>();
	const inFlight = new Map<number, AbortController>();

	const ok = (id: number, value: NonNullable<EngineReply["value"]>): void => {
		link.post({ v: 1, id, ok: true, value });
	};
	const fail = (id: number, error: unknown): void => {
		link.post({ v: 1, id, ok: false, error: toWireError(error) });
	};

	const off = link.subscribe((message) => {
		if (!isEngineRequest(message)) {
			return;
		}
		const request = message;
		if (request.op === "cancel") {
			// Fire-and-forget from the client: aborting is best-effort, and the
			// client's own rejection is what the UI actually sees.
			inFlight.get(request.id)?.abort();
			return;
		}

		const controller = new AbortController();
		inFlight.set(request.id, controller);
		const done = (): void => {
			inFlight.delete(request.id);
		};

		if (request.op === "search") {
			void runSearch(request, controller.signal, link, documents, requireEngine).then(
				done,
				(error: unknown) => {
					done();
					fail(request.id, error);
				},
			);
			return;
		}

		void runOne(request, controller.signal, documents, requireEngine, requireNavigation).then(
			(value) => {
				done();
				ok(request.id, value);
			},
			(error: unknown) => {
				done();
				fail(request.id, error);
			},
		);
	});

	/**
	 * Release everything this host is holding.
	 *
	 * Called both when the link is disposed deliberately and when the far end
	 * disconnects, because the two mean the same thing here: the page that owned
	 * the documents is gone, and nothing else is ever going to ask for them. An
	 * engine holding a parsed document whose viewer has vanished is a leak that
	 * only shows up as a memory ceiling some other document hits later.
	 */
	const release = (): void => {
		off();
		for (const controller of inFlight.values()) {
			controller.abort();
		}
		inFlight.clear();
		for (const doc of documents.values()) {
			void engine?.close(doc).catch(() => {
				// The page is gone; a close that fails here has nobody to tell.
			});
		}
		documents.clear();
	};

	link.onDisconnect(release);
	return release;
}

/** One non-streaming request. */
async function runOne(
	request: Exclude<EngineRequest, { op: "search" } | { op: "cancel" }>,
	signal: AbortSignal,
	documents: Map<string, DocHandle>,
	requireEngine: () => EnginePort,
	requireNavigation: () => NavigationCapability,
): Promise<NonNullable<EngineReply["value"]>> {
	switch (request.op) {
		case "open": {
			const descriptor: DocumentSourceDescriptor = {
				kind: "bytes",
				bytes: toArrayBuffer(decodeSource(request.src.bytes64)),
				name: request.src.name,
			};
			const doc = await requireEngine().open(descriptor, { signal });
			documents.set(doc.id, doc);
			const wire: WireDoc = {
				doc: doc.id,
				pageCount: doc.pageCount,
				pageSizes: doc.pageSizes.map((size) => ({
					width: size.width,
					height: size.height,
				})),
			};
			return wire;
		}
		case "close": {
			await requireEngine().close(requireDoc(documents, request.doc), { signal });
			documents.delete(request.doc);
			const ack: WireAck = { done: true };
			return ack;
		}
		case "render": {
			const tile = await requireEngine().renderTile(
				{
					doc: requireDoc(documents, request.doc),
					page: request.page,
					scale: request.scale,
					...(request.rect === undefined ? {} : { rect: request.rect }),
				},
				{ signal },
			);
			return {
				page: tile.page,
				width: tile.width,
				height: tile.height,
				format: "rgba8",
				bytes64: encodeBase64(new Uint8Array(tile.data)),
			};
		}
		case "text": {
			const page = await requireEngine().extractText(
				requireDoc(documents, request.doc),
				request.page,
				{
					signal,
				},
			);
			return { page: page.page, text: page.text };
		}
		// SL-4.EXT.03: the text layer's own op. It is mapped field by field
		// rather than cast, because the wire is JSON and a Rect that arrives
		// with a missing field is a highlight somewhere arbitrary that still
		// type-checks on the far side.
		case "textLayer": {
			const layer = await requireEngine().textLayer(
				requireDoc(documents, request.doc),
				request.page,
				{ signal },
			);
			return layer;
		}
		// ── SL-4.UI.06: the four navigation questions ──────────────────────
		//
		// The host is the authority on which documents exist and how many pages
		// they have, so it is the layer that refuses an out-of-range page: the
		// engine's own answer for one is `PAGE_OUT_OF_RANGE` (1301), a code
		// that talks about the engine's page numbering, and an empty list
		// would have been indistinguishable from "this page has no links".
		case "outline": {
			const doc = requireDoc(documents, request.doc);
			const report = await requireNavigation().outline(doc, { signal });
			return report;
		}
		case "pageLabels": {
			const doc = requireDoc(documents, request.doc);
			const ranges = await requireNavigation().pageLabels(doc, { signal });
			return { ranges };
		}
		case "destinations": {
			const doc = requireDoc(documents, request.doc);
			const destinations = await requireNavigation().destinations(doc, { signal });
			return { destinations };
		}
		case "pageLinks": {
			const doc = requireDoc(documents, request.doc);
			const reason = outOfRangePage(request.page, doc.pageCount);
			if (reason !== null) {
				throw AdapterError.badArgument(reason);
			}
			const links = await requireNavigation().pageLinks(doc, request.page, { signal });
			return { page: request.page, links };
		}
		default:
			// An op this host does not implement is a **capability** fact, not a
			// malformed request, so it answers 6017 rather than 6001 — the same
			// code the four navigation ops use when no capability is installed.
			// (Before SL-4.UI.06 the navigation ops fell through to here and
			// answered 6001, which said "your request was wrong" about a
			// request that was perfectly fine.)
			throw new AdapterError({
				code: ErrorCode.BindingUnsupportedOp,
				message: `the engine host does not implement '${(request as { op: string }).op}'`,
				docState: "Unchanged",
				retryable: false,
			});
	}
}

/**
 * A streaming request.
 *
 * Each batch is its own reply carrying the request's id, and the last carries
 * `done: true`. If the engine throws mid-stream the earlier batches have already
 * been delivered, so the failure is reported as a terminal `!ok` reply with the
 * same id: the client treats it as the rejection of the whole iterator, which is
 * what the UI expects from a failed search.
 */
async function runSearch(
	request: Extract<EngineRequest, { op: "search" }>,
	signal: AbortSignal,
	link: EngineLink,
	documents: Map<string, DocHandle>,
	requireEngine: () => EnginePort,
): Promise<void> {
	const batches: AsyncIterable<SearchBatch> = requireEngine().search(
		requireDoc(documents, request.doc),
		request.query,
		{
			caseSensitive: request.caseSensitive,
			wholeWord: request.wholeWord,
			fromPage: request.fromPage,
		},
		{ signal },
	);
	for await (const batch of batches) {
		const wire: WireSearchBatch = {
			matches: batch.matches.map((match) => ({
				page: match.page,
				start: match.start,
				end: match.end,
			})),
			progress: batch.progress,
			done: batch.done,
		};
		link.post({ v: 1, id: request.id, ok: true, value: wire });
	}
}

/** The handle for `id`, or `BINDING_BAD_HANDLE` naming the id that failed. */
function requireDoc(documents: Map<string, DocHandle>, id: string): DocHandle {
	const doc = documents.get(id);
	if (doc === undefined) {
		// `badHandle` and not a hand-built error: the shared contract suite
		// asserts `Unchanged` here, because a handle the engine never issued
		// means the document was never touched. `NotLoaded` would be a
		// different - and wrong - claim about the same failure.
		throw AdapterError.badHandle(`the engine host has no open document '${id}'`);
	}
	return doc;
}

function toArrayBuffer(bytes: Uint8Array): ArrayBuffer {
	const copy = new ArrayBuffer(bytes.byteLength);
	new Uint8Array(copy).set(bytes);
	return copy;
}

function decodeSource(bytes64: string): Uint8Array {
	try {
		return decodeBase64(bytes64);
	} catch (error) {
		throw AdapterError.badArgument(
			`the viewer sent document bytes that are not decodable (${base64ErrorReason(error)})`,
		);
	}
}

/** Any engine failure crosses the wire as its registry code, never as a bare string. */
function toWireError(error: unknown): WireError {
	if (error instanceof AdapterError) {
		return {
			code: error.code,
			message: error.message,
			docState: error.docState,
			retryable: error.retryable,
		};
	}
	return {
		code: ErrorCode.BindingBadHandle,
		message: error instanceof Error ? error.message : "the engine host failed",
		docState: "Unchanged",
		retryable: false,
	};
}
