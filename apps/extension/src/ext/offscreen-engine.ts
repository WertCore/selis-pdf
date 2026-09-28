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
	type WireAck,
	type WireDoc,
	type WireError,
	type WireSearchBatch,
	base64ErrorReason,
	decodeBase64,
	encodeBase64,
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
 */
export function serveEngineHost(options: {
	link: EngineLink;
	engine: EnginePort | null;
}): () => void {
	const { link, engine } = options;
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

		void runOne(request, controller.signal, documents, requireEngine).then(
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
		default:
			throw AdapterError.badArgument(
				`the engine host does not implement '${(request as { op: string }).op}'`,
			);
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
