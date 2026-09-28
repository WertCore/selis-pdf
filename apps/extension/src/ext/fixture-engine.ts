/**
 * SL-4.EXT.06 - the engine the transport is tested against, for tests only.
 *
 * **Not on `PACKAGE_ENTRIES` and must not be.** The bundled-only gate fails on
 * any file in the package without a ship-list row, so this cannot reach a store
 * upload by being next to the code that uses it.
 *
 * ## Why it delegates to `MockAdapter` instead of being an engine
 *
 * The thing under test here is the *transport*: base64 in both directions, id
 * correlation, a stream that terminates, cancellation that is prompt, and
 * registry codes that survive the crossing. Engine semantics are not under test
 * and must not be re-implemented here - a second engine in this repo is how two
 * engines start disagreeing about what a tile is.
 *
 * So the fixture reuses `apps/ui`'s own `createMockAdapter().engine` and only
 * supplies the one thing the wire cannot: the mock registers documents by
 * descriptor *identity*, and the host necessarily builds a fresh descriptor from
 * the bytes it decoded. Registering it on the way in is the whole adapter.
 * The sample document is the contract suite's, so the shared suite's own
 * expectations (page count, texts, match counts) hold unchanged.
 */

import type { EnginePort } from "../../../ui/src/platform/adapter.js";
import { SAMPLE_TEXTS } from "../../../ui/src/platform/contract.js";
import { createMockAdapter } from "../../../ui/src/platform/mock-adapter.js";
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

/** The three-page sample the shared contract suite probes. */
export const FIXTURE_PAGE_COUNT = SAMPLE_TEXTS.length;

/**
 * A `MockDocumentSpec` for the sample, minus the mock's own knowledge of it.
 *
 * Exported so a test that wants a *different* document can register one without
 * reaching past this module.
 */
export function createFixtureEngine(): EnginePort {
	const mock = createMockAdapter();

	return {
		async open(
			source: DocumentSourceDescriptor,
			options?: AdapterRequestOptions,
		): Promise<DocHandle> {
			if (source.kind !== "bytes") {
				// Anything else is a descriptor the mock has never seen, which is
				// exactly the contract suite's "unregistered descriptor" probe.
				return await mock.engine.open(source, options);
			}
			// The one line that makes the mock usable behind a wire: it keys its
			// registry by descriptor *identity* and `addDocument` returns the
			// object it registered, so the open has to use that one rather than
			// the descriptor the host rebuilt from the decoded bytes.
			const registered = mock.addDocument({
				name: source.name,
				pageCount: FIXTURE_PAGE_COUNT,
				textPages: SAMPLE_TEXTS,
				bytes: new Uint8Array(source.bytes),
			});
			return await mock.engine.open(registered, options);
		},
		close: (doc, options) => mock.engine.close(doc, options),
		renderTile: (
			request: RenderTileRequest,
			options?: AdapterRequestOptions,
		): Promise<RenderedTile> => mock.engine.renderTile(request, options),
		extractText: (
			doc: DocHandle,
			page: number,
			options?: AdapterRequestOptions,
		): Promise<PageText> => mock.engine.extractText(doc, page, options),
		search: (
			doc: DocHandle,
			query: string,
			searchOptions?: SearchOptions,
			requestOptions?: AdapterRequestOptions,
		): AsyncIterable<SearchBatch> => mock.engine.search(doc, query, searchOptions, requestOptions),
	};
}

/** A minimal in-memory document, for tests that only need bytes on the wire. */
export function sampleBytes(name = "sample.pdf"): Uint8Array {
	const header = `%PDF-1.4\n%${name}\n`;
	const bytes = new Uint8Array(header.length);
	for (let i = 0; i < header.length; i += 1) {
		bytes[i] = header.charCodeAt(i) & 0xff;
	}
	return bytes;
}
