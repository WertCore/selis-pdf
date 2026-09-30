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

/**
 * One UTF-16 code unit of a {@link TextLayerLine}'s text, with its selection
 * quad (SL-4.UI.04).
 *
 * The array is **index-aligned with the line's `text`**: `chars[i]` describes
 * `text[i]`. That is what makes a selection a pair of integer ranges and the
 * copied text a slice, and it is why the unit is a UTF-16 code unit rather than
 * a Unicode scalar — a JavaScript string is a UTF-16 sequence, so a
 * scalar-indexed array would desynchronise from it on the first astral
 * character. An astral character therefore occupies two entries: the first
 * carries the quad, the second is a zero-width, uninked continuation.
 */
export interface TextLayerChar {
	/** Selection quad in PDF user space (points, y-up, origin bottom-left). */
	readonly rect: Rect;
	/**
	 * The pen step to the next character along the writing direction, in points.
	 * Negative on a right-to-left line; zero on a continuation half and on a
	 * synthesised space. Signed on purpose: it is the only place the direction
	 * shows up in the data, and a viewer that sorts or measures by it gets
	 * right-to-left right for free.
	 */
	readonly advance: number;
	/**
	 * False for a character with no glyph of its own: a synthesised inter-word
	 * space (the assembler strips space glyphs when it splits words, so the text
	 * has characters the glyphs do not) and the trailing half of an astral
	 * scalar. Such a character still occupies an index, and its rect is the gap
	 * it stands for — which is what lets a selection across a word boundary
	 * include the space and a highlight over it cover the gap.
	 */
	readonly inked: boolean;
}

/**
 * One line of a page's text layer: its text, its box, and its characters.
 *
 * Lines arrive in **reading** order (the engine's SL-3.TEXT.04 answer:
 * structure-tree-first, geometry-fallback), not visual order. Copy walks them in
 * this order, which is the whole of "copy preserves reading order, not visual
 * order".
 */
export interface TextLayerLine {
	/** The recovered text of the line; words joined by single spaces. */
	readonly text: string;
	/** The line's bounding box, user space. */
	readonly rect: Rect;
	/**
	 * Which way this line's characters run, as the engine resolved it from the
	 * quads themselves. A host must not infer this from the text: a line of
	 * digits is left-to-right whatever the document's base direction is.
	 */
	readonly direction: "ltr" | "rtl";
	/** One entry per UTF-16 code unit of `text`, in order. */
	readonly chars: readonly TextLayerChar[];
}

/**
 * A page's text layer (SL-4.UI.04): character geometry in PDF user space.
 *
 * **User space, not device pixels, and not CSS pixels.** The layer is
 * resolution-independent, so one fetch serves every zoom level and a selection
 * made at one zoom is still exactly right at the next — the viewer multiplies
 * by the page's current CSS scale and by nothing else. Two traps this shape
 * exists to close:
 *
 * - Deriving text geometry from the compositor's tiles instead would couple
 *   selection to the render ladder, and the ladder deliberately presents a
 *   *previous* scale's bitmap during a zoom (`DrawOp.provisional`), so
 *   selection would be wrong exactly when it has to survive zooming.
 * - Multiplying by `SurfaceSize.devicePixelRatio` here would be a second,
 *   different mistake: that field is the *nominal* ratio, whereas the backing
 *   store's real factor is `SurfaceSize.scale` (`deviceWidth / cssWidth`).
 *   A text layer is DOM, positioned in CSS pixels inside the page tile, so it
 *   wants the page's CSS scale and must not know about device pixels at all.
 *
 * `text` is the page's text in reading order and is byte-identical to what
 * `selis extract --format=text` prints for the page — the DoD for copy is stated
 * against that, and a full-page copy is defined to reproduce it exactly.
 */
export interface PageTextLayer {
	readonly page: number;
	/** Page width in points; the box the layer's coordinates are relative to. */
	readonly width: number;
	/** Page height in points. */
	readonly height: number;
	/** Lines in reading order. */
	readonly lines: readonly TextLayerLine[];
	/** The page's text in reading order (low-confidence marker included). */
	readonly text: string;
	/**
	 * SL-3.TEXT.10: the page drew text the engine could not recover. A viewer
	 * should still show the layer (empty) and still copy the marker rather than
	 * an empty string that reads as a blank page.
	 */
	readonly lowConfidence: boolean;
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
	/**
	 * SL-3.TEXT.10: pages whose display list drew text but recovered none of it.
	 * They contribute no matches, so a non-empty list means `matches.length === 0`
	 * is *not* a clean "not found" — the query could not be answered on those
	 * pages, which is a different answer and must not be shown as one.
	 */
	readonly lowConfidencePages?: readonly number[];
}

/**
 * ## Navigation vocabulary (SL-4.UI.06)
 *
 * Outline, page labels, destinations and link annotations. These are *read
 * only* models: the viewer's navigation never mutates the document, so nothing
 * here carries a `write` path. They mirror `selis-pdf-doc`'s own shapes
 * (`page_labels`, `parse_destination`, the outline walk) so a transport is a
 * mapping rather than a translation, and every page number crossing the seam
 * is already **0-based** — the engine's `page_index` is 0-based too, and the
 * conversion to a 1-based "page 3" happens once, in the strings catalogue.
 */

/**
 * The numbering style of a `/PageLabels` range (PDF 32000-2:2020 §7.7.3.4,
 * Table 164). The names are the spec's: `D` decimal, `R`/`r` upper/lower roman,
 * `A`/`a` upper/lower alphabetic. A range with no `/S` is `D`.
 */
export type PageLabelStyle = "D" | "R" | "r" | "A" | "a";

/**
 * One `/PageLabels` number-tree entry: from `firstPage` onward, pages are
 * labelled `prefix` + the sequence number in `style`, counting from
 * `firstValue`. Absent members are the spec's defaults (`/St` 1, `/P` none,
 * `/S` `D`), and this shape says so by omission rather than by inventing
 * sentinel values a transport would have to special-case.
 */
export interface PageLabelRange {
	/** 0-based first page of the range. */
	readonly firstPage: number;
	readonly style?: PageLabelStyle;
	readonly prefix?: string;
	/** `/St`, the sequence's first value. Defaults to 1. */
	readonly firstValue?: number;
}

/**
 * The destination kinds the viewer acts on (PDF 32000-2:2020 §12.3.2.2).
 *
 * `/XYZ`, `/Fit`, `/FitH`, `/FitV`, `/FitR` and `/FitB` all name a *page*, and
 * the only thing the viewer does differently between them is where it places
 * that page and — for `/XYZ` — which point of it to show. Anything the viewer
 * does not recognise is carried as an `xyz` destination with no parameters,
 * which navigates to the page and nothing more; that is a loss of fidelity, not
 * an error, and it is recorded rather than guessed at.
 */
export type DestinationKind = "fit" | "fitH" | "fitV" | "fitR" | "fitB" | "xyz";

/** A resolved destination: a page, and how to place it. */
export interface PdfDestination {
	/** 0-based page index. */
	readonly page: number;
	readonly kind?: DestinationKind;
	/** `/XYZ left top zoom`; `left`/`top` are `null` when the author left them unset. */
	readonly left?: number | null;
	readonly top?: number | null;
	readonly zoom?: number | null;
}

/** One outline (bookmark) item, recursively. PDF 32000-2:2020 §12.3.3. */
export interface OutlineNode {
	/** The `/Title` as the document wrote it. Never localised, never trimmed. */
	readonly title: string;
	readonly children?: readonly OutlineNode[];
	/**
	 * The resolved `/Dest`, when the item points at one. An item can instead
	 * name a destination (`/Dest` as a name, or a `/A` `/GoTo` with a string),
	 * in which case `namedDestination` carries the name and the viewer resolves
	 * it against `NavigationPort.destinations`.
	 */
	readonly destination?: PdfDestination;
	readonly namedDestination?: string;
	/**
	 * `/Count` when the document set it. **Negative means the subtree starts
	 * collapsed**, which is the PDF convention (a positive count is the number
	 * of visible descendants) and the reason the viewer's initial expansion is
	 * a decision rather than a constant.
	 */
	readonly descendantCount?: number;
}

/** A `/Dests` name-tree entry: a name and the page it resolves to. */
export interface NamedDestination {
	readonly name: string;
	readonly destination: PdfDestination;
}

/**
 * The action classes a link annotation can carry (PDF 32000-2:2020 §12.6).
 *
 * The union is the whole vocabulary **including the classes ADR-P0020
 * disables**, because the transport reports what the document says and the
 * viewer decides what happens — a seam that dropped `/Launch` at the boundary
 * would make "is the viewer refusing this, or does the engine not know about
 * it?" unanswerable from the outside, which is exactly the question
 * `links.test.ts` exists to answer.
 */
export type LinkActionKind =
	| "goTo"
	| "uri"
	| "launch"
	| "goToR"
	| "submitForm"
	| "importData"
	| "javascript"
	| "named"
	| "none"
	| "unknown";

/** A link annotation's action, verbatim. */
export interface LinkAction {
	readonly kind: LinkActionKind;
	/**
	 * `/URI` for `uri`; the file specification for `launch`. Verbatim and
	 * unparsed: the scheme check is the viewer's (ADR-P0020), and a transport
	 * that normalised it first would hide what the document actually said.
	 */
	readonly uri?: string;
	readonly destination?: PdfDestination;
	/** The destination name for `named`, and for an outline item's `/Dest`. */
	readonly name?: string;
}

/** One link annotation on a page, with its quad in PDF user space. */
export interface LinkAnnotation {
	/** Opaque, document-scoped id; unique within a page. */
	readonly id: string;
	/** The annotation's rectangle in PDF user space (points, y-up). */
	readonly rect: Rect;
	readonly action: LinkAction;
	/** `/Contents` (the link's accessible name) when the document set one. */
	readonly contents?: string;
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
