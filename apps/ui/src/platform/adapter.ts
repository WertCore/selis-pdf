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
	PageText,
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

	/** Release host resources (workers, streams). Optional; shells may omit. */
	dispose?(): Promise<void>;
}
