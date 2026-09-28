/**
 * `PlatformAdapter` — the one seam between the Selis UI and its host
 * (SL-4.UI.01). Web (`apps/web/host`), the MV3 extension, desktop, and the
 * CLI/test harness each implement it; the UI code in `apps/ui` is forbidden
 * from touching platform globals directly (enforced by a unit-test lint gate
 * in this package) so the same bundle runs behind every adapter (ADR-P0022).
 *
 * Three seams, kept distinct:
 * 1. **Host seam** — this interface's OS/browser ports: files, storage,
 *    clipboard, print, telemetry, window/deep links.
 * 2. **Engine transport seam** — the {@link EnginePort}: open/render/text/
 *    search against the document engine. It is transport-agnostic; the
 *    WASM.01 Worker protocol plugs in behind it, a desktop Tauri command
 *    transport does the same, and the in-process mock serves tests.
 * 3. **Logic seam** — app state lives in `selis-viewmodel` (ADR-P0035) and
 *    the UI renders published diffs. It does *not* live here; conflating the
 *    host seam with app state is how logic leaks back into the shell.
 *
 * Conventions:
 * - Every port method is async and never blocks; long operations take
 *   {@link AdapterRequestOptions} with an `AbortSignal` and an optional
 *   progress callback.
 * - Every rejection is an {@link AdapterError} carrying a registry code and
 *   `docState` (what happened to the user's document).
 * - Cancellation is always available and surfaces as code 4020
 *   (`CANCELLED`, `docState: "Unchanged"`).
 * - The UI must never phone home: telemetry is opt-in (ADR-P0017), document
 *   data never crosses the network without explicit cloud consent
 *   (ADR-P0016), and `http-range` sources stream bytes into the local engine
 *   — nothing is uploaded.
 */

import type {
	AdapterRequestOptions,
	BudgetProfile,
	DeepLink,
	DocHandle,
	DocumentSourceDescriptor,
	LinkAnnotation,
	NamedDestination,
	OutlineNode,
	PageLabelRange,
	PageText,
	PageTextLayer,
	PlatformCapabilities,
	PrintOptions,
	RenderTileRequest,
	RenderedTile,
	SaveTarget,
	SearchBatch,
	SearchOptions,
	TelemetryEvent,
} from "./types.js";

/** Document engine operations. See the module doc for the transport contract. */
export interface EnginePort {
	/**
	 * Open a document source. The transport decides when bytes are read —
	 * a Worker may pull ranges lazily; the mock reads nothing. The returned
	 * handle is only valid against this adapter instance.
	 *
	 * Progress: `onProgress` reports open stages. Cancellation aborts the
	 * open and leaves nothing loaded (`docState: "NotLoaded"` on failure).
	 */
	open(
		source: DocumentSourceDescriptor,
		options?: AdapterRequestOptions & { budget?: BudgetProfile },
	): Promise<DocHandle>;

	/**
	 * Release the document. Handles die with the adapter anyway; explicit
	 * close lets the transport free engine memory deterministically
	 * (24-BINDINGS-SPEC §2, memory strategy).
	 */
	close(doc: DocHandle, options?: AdapterRequestOptions): Promise<void>;

	/**
	 * Render one tile. `request.rect` tiles a page; omit it for the whole
	 * page. The returned RGBA8 buffer is a copy owned by the caller —
	 * transports may deliver zero-copy internally but never alias engine
	 * memory after the promise resolves.
	 */
	renderTile(request: RenderTileRequest, options?: AdapterRequestOptions): Promise<RenderedTile>;

	/** Extract one page's text in engine reading order. */
	extractText(doc: DocHandle, page: number, options?: AdapterRequestOptions): Promise<PageText>;

	/**
	 * Fetch one page's text layer: per-character selection quads in PDF user
	 * space, with the recovered text, in engine reading order (SL-4.UI.04).
	 *
	 * **Why this exists beside `extractText`, and why it is not the tiles.**
	 * `extractText` answers "what does the page say"; the text layer answers
	 * "where is each character", which is a different question with a different
	 * cost model. Three properties make the split deliberate rather than
	 * accidental:
	 *
	 * 1. **The glyphs are the only source of character geometry.** A rendered
	 *    tile has pixels and no characters. Deriving a text layer from tiles
	 *    would couple selection to the render ladder, and UI.03's ladder
	 *    deliberately presents a *previous* scale's bitmap during a zoom
	 *    (`DrawOp.provisional`) — so selection would be wrong exactly during the
	 *    zoom it has to survive.
	 * 2. **The result is scale-free.** The quads are in PDF points, so one fetch
	 *    serves every zoom level and a selection made at one zoom is still
	 *    exactly right at the next. A selection is stored as character indices
	 *    and re-projected, never as pixels.
	 * 3. **It is resolution-independent of the surface.** The caller multiplies
	 *    by the page's *CSS* scale. `SurfaceSize.scale` (the backing store's
	 *    real device factor) and `SurfaceSize.devicePixelRatio` (the nominal
	 *    one) are both canvas concepts and must not appear here — the layer is
	 *    DOM positioned in CSS pixels.
	 *
	 * Transports map this onto whatever their engine offers: the WASM worker
	 *    protocol gets a `textLayer` op, the desktop transport a Tauri command.
	 *    A transport that cannot supply quads must reject rather than invent
	 *    them — a text layer positioned by guesswork is a selection that
	 *    highlights the wrong words, which is worse than no selection.
	 */
	textLayer(doc: DocHandle, page: number, options?: AdapterRequestOptions): Promise<PageTextLayer>;

	/**
	 * Search the whole document progressively. Batches stream page-by-page;
	 * the final batch carries `done: true`. Cancelling mid-scan surfaces
	 * `CANCELLED`; matches already yielded stay valid.
	 */
	search(
		doc: DocHandle,
		query: string,
		searchOptions?: SearchOptions,
		requestOptions?: AdapterRequestOptions,
	): AsyncIterable<SearchBatch>;
}

/**
 * Navigation: the document structures that are not pixels and not text
 * (SL-4.UI.06) — the outline (bookmarks), `/PageLabels`, the `/Dests` name
 * tree, and a page's link annotations.
 *
 * ## Why this is a *separate port* and not four more `EnginePort` methods
 *
 * Three reasons, and the third is the one that would bite:
 *
 * 1. **It is optional, and optionality has to live somewhere structural.** The
 *    WASM.01 protocol (`crates/selis-pdf-wasm/src/protocol.rs`) has ops for
 *    open/render/text/textLayer/search and nothing for outlines or links, and
 *    the engine's outline walk is not merged. A port whose methods are required
 *    would have been satisfied today only by every transport answering
 *    `BINDING_UNSUPPORTED_OP` — a permanent, indistinguishable "no". Declaring
 *    `PlatformAdapter.navigation` optional says what is actually true: a host
 *    without it has **no** outline, **no** labels and **no** links, and the
 *    viewer says so in its published state instead of showing an empty panel
 *    that looks like a broken document.
 * 2. **The cost model differs.** Everything in `EnginePort` is either a render
 *    or a per-page scan, cancellable mid-flight. The outline is a whole-tree
 *    walk that can be pathological (a 2 000-item outline, each item with 2 000
 *    children), so it belongs behind its own budget argument rather than
 *    sharing `renderTile`'s.
 * 3. **Activation is a viewer decision, not a transport one.** ADR-P0020 makes
 *    *what happens* to a `/Launch` or a `javascript:` URI a policy of this
 *    product. A port that also decided would put the policy on the wrong side
 *    of a boundary the extension and the desktop shell both implement, and
 *    would make "does the desktop viewer prompt where the extension refuses?"
 *    a question about two Rust crates instead of one function.
 *
 * So this port **reports what the document says**, including the action
 * classes ADR-P0020 disables, and `viewer/links.ts` decides what happens. The
 * tests in `viewer/links.test.ts` drive the decision and assert the disabled
 * classes cannot be reached.
 */
export interface NavigationPort {
	/**
	 * The document's outline (bookmark) tree, in document order.
	 *
	 * An unresolvable item (a `/Dest` naming a destination that is not in the
	 * name tree) is still returned, with `namedDestination` set and no
	 * `destination`: a bookmark the reader can see and that reports "this
	 * document's destination is missing" is more useful, and more honest, than
	 * a silently shorter outline.
	 */
	outline(doc: DocHandle, options?: AdapterRequestOptions): Promise<readonly OutlineNode[]>;

	/**
	 * The `/PageLabels` ranges, in document order. Empty for a document with
	 * no labels tree, which the viewer reads as "use the page number".
	 */
	pageLabels(doc: DocHandle, options?: AdapterRequestOptions): Promise<readonly PageLabelRange[]>;

	/**
	 * The `/Dests` name tree. Used to resolve named destinations, in outline
	 * items and in `/GoTo` link actions alike.
	 */
	destinations(
		doc: DocHandle,
		options?: AdapterRequestOptions,
	): Promise<readonly NamedDestination[]>;

	/**
	 * One page's link annotations, in annotation-array order.
	 *
	 * Out-of-range pages reject with `BINDING_BAD_ARGUMENT`, like every other
	 * per-page port method — a transport that returned an empty list for page
	 * 9 000 of a 3-page document would be indistinguishable from a page with
	 * no links.
	 */
	pageLinks(
		doc: DocHandle,
		page: number,
		options?: AdapterRequestOptions,
	): Promise<readonly LinkAnnotation[]>;
}

/** Host file dialogs. The viewer is read-only; save targets exist for Phase 5. */
export interface FilePort {
	/** Open the host picker. Resolves to the descriptors the user picked. */
	pickOpen(options?: PickOpenOptions): Promise<readonly DocumentSourceDescriptor[]>;
	/** Choose a save target. `null` means the user cancelled. */
	pickSave(suggestedName: string): Promise<SaveTarget | null>;
}

/** Options for {@link FilePort.pickOpen}. */
export interface PickOpenOptions {
	/** Accepted extensions/mime hints, e.g. `[".pdf", "application/pdf"]`. */
	readonly accept?: readonly string[];
	readonly multiple?: boolean;
}

/** Key/value persistence for small settings: theme, density, recent list. */
export interface StoragePort {
	get(key: string): Promise<string | null>;
	set(key: string, value: string): Promise<void>;
	delete(key: string): Promise<void>;
}

/** Text clipboard. */
export interface ClipboardPort {
	writeText(text: string): Promise<void>;
	/** Throws when `capabilities.clipboardRead` is false. */
	readText(): Promise<string>;
}

/** Print trigger. Print rendering itself is SL-4.UI.08. */
export interface PrintPort {
	print(doc: DocHandle, options?: PrintOptions): Promise<void>;
}

/**
 * Opt-in telemetry sink (ADR-P0017). Off by default. `record` ignores events
 * unless the user opted in, and {@link TelemetryEvent} has no field that can
 * carry document bytes, text, names, or URLs — the type is the boundary.
 */
export interface TelemetryPort {
	isEnabled(): boolean;
	setEnabled(enabled: boolean): Promise<void>;
	record(event: TelemetryEvent): void;
}

/** Window/menu integration and deep links. */
export interface WindowPort {
	/** Set the window title (document name + product, host decides format). */
	setTitle(title: string): Promise<void>;
	/**
	 * Open a URL outside the app. The UI must prompt with the full
	 * destination first and refuse document-internal launch classes
	 * (ADR-P0020); this port only performs the host action.
	 */
	openExternal(url: string): Promise<void>;
	/** Subscribe to deep links routed by the host. Returns an unsubscribe. */
	onDeepLink(handler: (link: DeepLink) => void): () => void;
}

/**
 * Which language the reader wants (SL-4.UI.11).
 *
 * A **host** fact, and the reason this is a port rather than a line in
 * `apps/ui`: the answer lives in `navigator.language`, an OS setting, or a
 * build-time constant, and reading any of them from the UI is exactly what
 * `platform-globals.test.ts` exists to prevent. The host already owns every
 * other environment fact for the same reason.
 *
 * It is **read-only on purpose**. A locale the *user* chose is a setting, and
 * settings go through {@link StoragePort} (the theme and density already do, see
 * UI.10); this port reports the preference that choice overrides. Letting the
 * UI write the tag here as well would put the user's choice in two stores that
 * could disagree, and the loser would be the locale the reader sees.
 *
 * Optional, and absent is the honest default: a host that has no answer (a test
 * harness, a service worker with no UI) simply reports no port and the viewer
 * ships English. `negotiateLocale` then does the rest — see
 * `i18n/README.md`.
 */
export interface LocalePort {
	/**
	 * The host's current preference as a BCP 47 tag (`"en"`, `"de-AT"`).
	 *
	 * A tag the build does not ship is not an error: `negotiateLocale` resolves
	 * it to the closest locale that does, and falls back to the source.
	 */
	current(): string;
	/**
	 * Subscribe to the preference changing — a user switching the OS language,
	 * or a host that offers a language picker.
	 *
	 * The handler receives the new tag and nothing else: applying it is the
	 * shell's job (rebuild the runtime, repaint), which keeps the runtime a pure
	 * function of its catalogues. Returns an unsubscribe.
	 */
	onChange(handler: (tag: string) => void): () => void;
}

/**
 * The host seam. UI code receives one of these at startup and threads it
 * down; it never imports a concrete adapter — composition (which adapter to
 * install) belongs to each shell's entry point, outside `apps/ui`.
 */
export interface PlatformAdapter {
	/** Fixed capability flags for this host session. */
	readonly capabilities: PlatformCapabilities;

	/** Document engine operations (transport-agnostic; see module doc). */
	readonly engine: EnginePort;

	/** Host file dialogs: pick documents to open, choose save targets. */
	readonly files: FilePort;

	/** Key/value persistence for small settings. */
	readonly storage: StoragePort;

	/** Text clipboard. */
	readonly clipboard: ClipboardPort;

	/** Print trigger. */
	readonly print: PrintPort;

	/** Opt-in telemetry sink. Off by default; never carries document data. */
	readonly telemetry: TelemetryPort;

	/** Window title, external links, deep links. */
	readonly window: WindowPort;

	/**
	 * Document navigation structures (SL-4.UI.06), or `undefined` when the host
	 * cannot supply them.
	 *
	 * Optional because it is genuinely absent today: the WASM.01 worker protocol
	 * has no outline/links op and the engine's outline walk is not merged, so
	 * `apps/web/host` and the extension both report `undefined` until a later
	 * engine task lands. That is the honest shape, and it is why a required port
	 * would have been wrong — see {@link NavigationPort}.
	 *
	 * The viewer treats `undefined` as "this host has no outline, no page
	 * labels and no link annotations", publishes that in its state, and offers
	 * the reader nothing it cannot honour. It never falls back to reading the
	 * bytes itself.
	 */
	readonly navigation?: NavigationPort;

	/**
	 * The reader's language preference (SL-4.UI.11), or `undefined` when the
	 * host has no answer.
	 *
	 * Optional for the same reason {@link navigation} is: a port a host cannot
	 * fill should be absent rather than faked, and "this host cannot say what
	 * language the reader wants" is a real state for a harness and for a
	 * service worker with no UI. The viewer reads it once at startup, resolves
	 * it through `negotiateLocale` against the catalogues the build actually
	 * ships, and renders English when there is nothing to resolve to. It never
	 * sniffs the environment itself (ADR-P0022: one UI, every host).
	 */
	readonly locale?: LocalePort;

	/** Release host resources (workers, streams). Optional; shells may omit. */
	dispose?(): Promise<void>;
}
