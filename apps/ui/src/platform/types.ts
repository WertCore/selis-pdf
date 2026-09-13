/**
 * Shared request/result vocabulary for the platform seam. Everything here is
 * transport-agnostic: it describes *what* the UI needs, never *how* a host or
 * a worker protocol delivers it. The WASM.01 Worker protocol maps onto these
 * types from the outside; nothing here is a wire format.
 */

/** PDF user-space size in points. */
export interface Size {
	readonly width: number;
	readonly height: number;
}

/** Axis-aligned rectangle in PDF user space (points, origin bottom-left). */
export interface Rect {
	readonly x: number;
	readonly y: number;
	readonly width: number;
	readonly height: number;
}

/**
 * Where a document comes from. Deliberately independent of the WASM.01
 * `SourceDescriptor` wire shape — transports map it. The names mirror the
 * bindings spec's adapter set (`blob | opfs | fsa | http-range | bytes`).
 */
export type DocumentSourceDescriptor =
	/** Fully in-memory bytes (drag-drop buffers, fixtures, CLI stdin). */
	| { readonly kind: "bytes"; readonly bytes: ArrayBuffer; readonly name: string }
	/** A browser `File`/`Blob` picked by the host (never read on the UI thread). */
	| { readonly kind: "blob"; readonly blob: Blob; readonly name: string }
	/** A path inside the origin-private OPFS. */
	| { readonly kind: "opfs"; readonly path: string }
	/** A File System Access handle (save-in-place, Chrome-family hosts). */
	| { readonly kind: "fsa"; readonly handle: FsaFileHandle }
	/** A remote PDF opened by HTTP range requests — local processing, no upload. */
	| { readonly kind: "http-range"; readonly url: string; readonly length?: number }
	/** A native filesystem path (desktop shell, CLI, tests). */
	| { readonly kind: "file"; readonly path: string };

/**
 * Minimal structural shape of a File System Access handle so this package
 * compiles without the DOM lib. A real `FileSystemFileHandle` satisfies it.
 */
export interface FsaFileHandle {
	readonly kind: "file";
	readonly name: string;
}

/**
 * Resource ceilings for an open/render, mirroring the engine's `Budget`
 * vocabulary (ADR-P0006). Shells pick a profile; `selis-policy` owns defaults.
 * Exhaustion surfaces as an `AdapterError` with the matching 40xx code.
 */
export interface BudgetProfile {
	readonly bytes?: number;
	readonly wallMs?: number;
	readonly objects?: number;
	readonly pixels?: number;
}

/** Progress report; `fraction` is 0..1, `stage` is a coarse machine label. */
export interface ProgressReport {
	readonly fraction: number;
	readonly stage: string;
}

/**
 * Per-request controls. Cancellation uses `AbortSignal` — transports map it
 * to their cancel primitive (the WASM.01 `cancel` message, a native
 * `CancelToken`, …). Cancellation must be observable promptly and surfaces as
 * `AdapterError` with code 4020 (`CANCELLED`) and `docState: "Unchanged"`.
 */
export interface AdapterRequestOptions {
	readonly signal?: AbortSignal;
	readonly onProgress?: (progress: ProgressReport) => void;
	/** Resource ceiling override for this request. */
	readonly budget?: BudgetProfile;
}

/** An open document. Opaque beyond the metadata the UI needs for layout. */
export interface DocHandle {
	/** Adapter-issued identifier; meaningless across sessions. */
	readonly id: string;
	/** Number of pages, after open. */
	readonly pageCount: number;
	/** Per-page media sizes in points, index-aligned with page numbers. */
	readonly pageSizes: readonly Size[];
}

/** What the UI asks the engine to rasterise. */
export interface RenderTileRequest {
	readonly doc: DocHandle;
	/** 0-based page index. */
	readonly page: number;
	/** Device pixels per PDF point (zoom × devicePixelRatio is the UI's call). */
	readonly scale: number;
	/** Sub-rect to render in page space; omit for the whole page. */
	readonly rect?: Rect;
	/** Coarse quality intent so the engine can pick a budget profile. */
	readonly hint?: "thumbnail" | "view" | "print";
}

/** A rendered tile. RGBA8 rows top-down, tightly packed, no stride padding. */
export interface RenderedTile {
	readonly page: number;
	readonly width: number;
	readonly height: number;
	readonly format: "rgba8";
	readonly data: ArrayBuffer;
}

/** Extracted text of one page, in engine reading order (SL-3.TEXT.05). */
export interface PageText {
	readonly page: number;
	readonly text: string;
}

/** Search modifiers; defaults are case-insensitive substring matching. */
export interface SearchOptions {
	readonly caseSensitive?: boolean;
	readonly wholeWord?: boolean;
	/** Page to start from (default 0). */
	readonly fromPage?: number;
}

/** One match: character range within the page's extracted text. */
export interface SearchMatch {
	readonly page: number;
	readonly start: number;
	readonly end: number;
	/** Glyph quads when the engine exposes them (SL-3.TEXT.06); optional. */
	readonly rects?: readonly Rect[];
}

/**
 * A progressive slice of search results. Batches arrive page-by-page as the
 * engine scans; the final batch carries `done: true` and `progress: 1`.
 */
export interface SearchBatch {
	readonly matches: readonly SearchMatch[];
	/** How much of the document has been scanned, 0..1. */
	readonly progress: number;
	/** True only for the final batch. */
	readonly done: boolean;
}

/** Target for a future save (Phase 5); the read-only viewer never calls it. */
export interface SaveTarget {
	readonly name: string;
	write(bytes: Uint8Array): Promise<void>;
}

/** Options for the host open dialog. */
export interface PickOpenOptions {
	readonly accept?: readonly string[];
	readonly multiple?: boolean;
}

/** Print request; the actual print rendering is SL-4.UI.08's job. */
export interface PrintOptions {
	readonly pages?: readonly number[];
	readonly copies?: number;
}

/**
 * Telemetry event. **Type-level privacy boundary (ADR-P0017):** there is no
 * field that could carry document bytes, text, names, or URLs — only an event
 * name and numeric counters. Events are ignored unless the user opted in.
 */
export interface TelemetryEvent {
	readonly name: string;
	readonly metrics?: Readonly<Record<string, number>>;
}

/** A deep link the host routed into the app (e.g. `selis://open?…`). */
export interface DeepLink {
	readonly url: string;
}

/**
 * Static capability flags. The UI reads these to enable affordances; it never
 * feature-sniffs the host directly. All flags are fixed per host session.
 */
export interface PlatformCapabilities {
	/** Which shell is hosting this adapter instance. */
	readonly platform: "web" | "extension" | "desktop" | "cli" | "mock";
	/** Host file open/save dialogs are available. */
	readonly filePickers: boolean;
	/** File System Access handles (save-in-place) are available. */
	readonly fileSystemAccess: boolean;
	/** Origin-private OPFS is available. */
	readonly opfs: boolean;
	/** HTTP range sources are supported (viewer still never uploads; ADR-P0016). */
	readonly httpRange: boolean;
	/** Clipboard read is available (write is always assumed available). */
	readonly clipboardRead: boolean;
	/** A print path is available (wired up by SL-4.UI.08). */
	readonly print: boolean;
	/** KV storage survives sessions. */
	readonly persistedStorage: boolean;
	/** Cross-origin isolation is present (SharedArrayBuffer/threads usable). */
	readonly threads: boolean;
	/** The host routes deep links into the app. */
	readonly deepLinks: boolean;
}
