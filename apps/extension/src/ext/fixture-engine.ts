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
 *
 * ## The navigation half (SL-4.UI.06)
 *
 * The sample document carries an outline, `/PageLabels`, a named destination
 * and two link annotations - one of them a `/Launch`, because the transport's
 * job is to report the classes ADR-0020 disables and a fixture that only
 * carried safe ones would not prove that.
 *
 * The mock answers the outline with the UI's *list*; the capability the host
 * serves answers with the engine's *report*, because the wire carries the
 * difference between "this document has no outline" and "this document has an
 * outline this engine could not read in full". The fixture bridges the two,
 * and the bridge is the only place in this package where the two vocabularies
 * meet.
 */

import type { EnginePort, NavigationPort } from "../../../ui/src/platform/adapter.js";
import { SAMPLE_TEXTS } from "../../../ui/src/platform/contract.js";
import { type PageLinkSet, createMockAdapter } from "../../../ui/src/platform/mock-adapter.js";
import type {
	AdapterRequestOptions,
	DocHandle,
	DocumentSourceDescriptor,
	NamedDestination,
	OutlineNode,
	PageLabelRange,
	PageText,
	PageTextLayer,
	RenderTileRequest,
	RenderedTile,
	SearchBatch,
	SearchOptions,
} from "../../../ui/src/platform/types.js";
import type { OutlineReport } from "../../../ui/src/viewer/navigation-wire.js";
import type { NavigationCapability } from "./engine-protocol.js";

/** The three-page sample the shared contract suite probes. */
export const FIXTURE_PAGE_COUNT = SAMPLE_TEXTS.length;

/**
 * The sample document's outline.
 *
 * Two chapters, one of them with a child, and one item that names a
 * destination rather than carrying one - the three shapes a bookmark can
 * have, so the transport is exercised on all of them.
 */
export const FIXTURE_OUTLINE: readonly OutlineNode[] = [
	{
		title: "Chapter one",
		destination: { page: 0, kind: "fit" },
		descendantCount: -1,
		children: [{ title: "Figure 1", destination: { page: 1, kind: "xyz", top: 700 } }],
	},
	{ title: "Appendix", namedDestination: "here" },
];

/** The sample document's `/PageLabels`: roman for the front matter, arabic after. */
export const FIXTURE_PAGE_LABELS: readonly PageLabelRange[] = [
	{ firstPage: 0, style: "r", prefix: "p" },
	{ firstPage: 2, style: "D", firstValue: 7 },
];

/** The sample document's `/Dests` name tree. */
export const FIXTURE_DESTINATIONS: readonly NamedDestination[] = [
	{ name: "here", destination: { page: 2 } },
];

/**
 * The sample document's link annotations.
 *
 * The second one is a `/Launch` with a local path as its target: ADR-0020
 * refuses that class in the **viewer**, and the refusal is only testable if
 * the transport carries the class across verbatim.
 */
export const FIXTURE_LINKS: readonly PageLinkSet[] = [
	{
		page: 0,
		links: [
			{
				rect: { x: 72, y: 700, width: 100, height: 12 },
				action: { kind: "uri", uri: "https://example.com/a" },
			},
			{
				id: "launch-1",
				rect: { x: 72, y: 680, width: 100, height: 12 },
				action: { kind: "launch", uri: "C:/payload.exe" },
			},
		],
	},
];

/**
 * A `MockDocumentSpec` for the sample, minus the mock's own knowledge of it.
 *
 * Exported so a test that wants a *different* document can register one without
 * reaching past this module.
 */
export function createFixtureEngine(): EnginePort & NavigationCapability {
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
				// The navigation data the shared contract suite's navigation
				// probes read. The suite asserts *shape* rather than content,
				// so this is enough to prove the four reads cross the wire.
				outline: FIXTURE_OUTLINE,
				pageLabels: FIXTURE_PAGE_LABELS,
				destinations: FIXTURE_DESTINATIONS,
				links: FIXTURE_LINKS,
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
		// UI.04 made `EnginePort.textLayer` required; EXT.06 was developed
		// against the previous shape. The mock already implements it, so the
		// fixture forwards rather than inventing a second geometry source.
		textLayer: (
			doc: DocHandle,
			page: number,
			options?: AdapterRequestOptions,
		): Promise<PageTextLayer> => mock.engine.textLayer(doc, page, options),
		search: (
			doc: DocHandle,
			query: string,
			searchOptions?: SearchOptions,
			requestOptions?: AdapterRequestOptions,
		): AsyncIterable<SearchBatch> => mock.engine.search(doc, query, searchOptions, requestOptions),

		// ── SL-4.UI.06: the navigation capability ──────────────────────────
		//
		// The mock's navigation port is the UI's shape; the capability is the
		// engine's. The outline is the one answer that differs, so it is the
		// one that needs the bridge.
		outline: async (doc, options): Promise<OutlineReport> => {
			const items = await requireMockNavigation(mock).outline(doc, options);
			return {
				present: items.length > 0,
				// The mock never truncates and never prunes: it is not an
				// engine, and claiming otherwise would be a claim about a
				// document the mock knows nothing about.
				truncated: false,
				pruned: false,
				items,
			};
		},
		pageLabels: (doc, options) => requireMockNavigation(mock).pageLabels(doc, options),
		destinations: (doc, options) => requireMockNavigation(mock).destinations(doc, options),
		pageLinks: (doc, page, options) => requireMockNavigation(mock).pageLinks(doc, page, options),
	};
}

/**
 * The mock's navigation port.
 *
 * The mock always has one - `navigation` is not optional on it - but the type
 * says it is, because `PlatformAdapter.navigation` is. Refusing rather than
 * answering `[]` keeps a future mock without the port from looking like a
 * document with no outline.
 */
function requireMockNavigation(mock: ReturnType<typeof createMockAdapter>): NavigationPort {
	const navigation = mock.navigation;
	if (navigation === undefined) {
		throw new Error("the mock adapter has no navigation port");
	}
	return navigation;
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
