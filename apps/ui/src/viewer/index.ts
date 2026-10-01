/**
 * The virtualised page list — SL-4.UI.02.
 *
 * Layered so each file can be tested (and reasoned about) on its own:
 * `layout` (pure geometry) → `windowing` (which rows exist) →
 * `anchoring` (where the reader stays across a relayout) →
 * `tile-ladder` (what the engine is asked to rasterise) →
 * `keyboard` (the a11y key map) → `page-list` (the controller that owns the
 * state and hands the shell one immutable value to paint).
 *
 * `page-list.css` is the component's presentation; it is not exported as a
 * string because the repo ships CSS as files (ADR-P0021: no bundler-runtime
 * styling) — shells import `@selis/ui/css/page-list.css` after
 * `@selis/ui-kit/css/tokens.css` and `base.css`.
 */

export type {
	LayoutRequest,
	PageFitMode,
	PageLayout,
	PlacedPage,
	PlacedRow,
} from "./layout.js";
export {
	MAX_SCALE,
	MIN_SCALE,
	columnsFor,
	layoutPages,
	pageAt,
	pageAtScrollTop,
	planRows,
	rowIndexAt,
} from "./layout.js";

export type { PageWindow, WindowRequest } from "./windowing.js";
export {
	DEFAULT_OVERSCAN_PX,
	MAX_WINDOW_ROWS,
	clampScrollTop,
	prefetchRows,
	scrollTopForPage,
	visibleWindow,
} from "./windowing.js";

export type { ScrollAnchor } from "./anchoring.js";
export { anchorScrollTop, captureAnchor, proportionalScrollTop } from "./anchoring.js";

export type {
	LadderInput,
	RenderedStage,
	TileEntry,
	TileSchedulerOptions,
	TileStage,
	TileTask,
} from "./tile-ladder.js";
export {
	LOW_RES_DEVICE_SCALE,
	MAX_CACHED_TILES,
	MAX_CONCURRENT_TILES,
	TileScheduler,
	planLadder,
	taskKey,
	tileKey,
} from "./tile-ladder.js";

export type {
	CompositorFrame,
	DrawOp,
	FrameClock,
	FrameHandle,
	SurfaceSize,
	TileSurface,
} from "./surface.js";

export type {
	PresentedFrame,
	TileCompositor,
	TileCompositorOptions,
} from "./compositor.js";
export {
	createTileCompositor,
	effectiveDeviceScale,
	isAtScale,
	pickTile,
	surfaceSizeFor,
} from "./compositor.js";

export type {
	CompositorContext,
	FrameOutcome,
	OffscreenCompositor,
	OffscreenCompositorOptions,
	OffscreenTarget,
	RecordedDraw,
} from "./offscreen-compositor.js";
export { createOffscreenCompositor } from "./offscreen-compositor.js";

export type {
	FrameRequester,
	SurfaceCommand,
	SurfaceDrawOp,
	SurfaceFrame,
	SurfaceFrameEvent,
	WorkerSurface,
	WorkerSurfaceOptions,
} from "./worker-surface.js";
export {
	SURFACE_PROTOCOL,
	commandTransfer,
	createAnimationFrameClock,
	createWorkerSurface,
	serialiseFrame,
	surfaceOpKey,
} from "./worker-surface.js";

export type { KeyboardContext, PageListKey, PageNavigationCommand } from "./keyboard.js";
export { announcement, pageLabel, resolvePageKey, stepFor } from "./keyboard.js";

export type { CharBox, LayerLineBox, TextLayerFrame } from "./text-layer.js";
export { buildTextLayer, caretX, lineLength } from "./text-layer.js";

export type {
	Caret,
	CaretMove,
	CopyRequest,
	OrderedRange,
	SelectionRange,
} from "./selection.js";
export {
	caretFromPoint,
	clampCaret,
	collapsedAt,
	copyText,
	documentEnd,
	documentStart,
	expandToWord,
	isCollapsed,
	moveCaret,
	moveRange,
	orderedRange,
	selectedText,
	selectionRects,
} from "./selection.js";

export type {
	SearchCommand,
	SearchCommandAction,
	SearchKey,
	SearchKeyContext,
} from "./keyboard.js";
export { resolveSearchKey } from "./keyboard.js";

export type {
	PageMatchCount,
	SearchController,
	SearchControllerOptions,
	SearchHighlight,
	SearchMatchRange,
	SearchModifiers,
	SearchState,
	SearchStatus,
	SearchWindow,
} from "./search.js";
export { caretAtTextIndex, createSearch, matchCarets } from "./search.js";

export type {
	PageList,
	PageListMetrics,
	PageListOptions,
	PageListState,
	PageListViewport,
	PageTileView,
} from "./page-list.js";
export { DEFAULT_METRICS, createPageList } from "./page-list.js";

export type { NormalisedLabelRange } from "./page-labels.js";
export type { HealthReport, HealthRow, HealthSeverity } from "./health.js";
export { healthReportFromWire, healthRows, summariseDeviations, worstSeverity } from "./health.js";

export type { ErrorKind, RegistryCode } from "./error-codes.js";
export { REGISTRY, REGISTRY_SIZE } from "./error-codes.js";
export type { ErrorSeverity, ErrorState, RecoveryAction } from "./errors.js";
export { codesByKind, errorState, everyErrorState, recoveryFor, severityFor } from "./errors.js";
export type { FailurePanel, FailureStrings } from "./error-panel.js";
export { failurePanel, failurePanelFromWire } from "./error-panel.js";
export {
	DEFAULT_LABEL_STYLE,
	MAX_ROMAN,
	formatSequenceValue,
	hasMeaningfulLabels,
	normaliseLabelRanges,
	pageLabelFor,
	pageLabelsFor,
	rangeForPage,
	toAlphabetic,
	toRoman,
} from "./page-labels.js";

export type { FlatOutline, FlattenRequest, OutlineKey, OutlineRow } from "./outline.js";
export {
	MAX_OUTLINE_DEPTH,
	MAX_OUTLINE_ITEMS,
	MAX_OUTLINE_ROWS,
	expandSubtree,
	flattenOutline,
	hasOutline,
	initialExpansion,
	moveOutlineFocus,
	parentIdsOf,
	rowPage,
} from "./outline.js";

export type { LinkDecision, LinkRefusal } from "./links.js";
export {
	ALLOWED_EXTERNAL_SCHEMES,
	REFUSAL_FOR_ACTION,
	alignmentForDestination,
	decideLinkAction,
	hostOf,
	isAllowedExternalScheme,
	schemeOf,
} from "./links.js";

export type {
	LinkActivation,
	NavigationController,
	NavigationOptions,
	NavigationState,
	NavigationTarget,
	PendingExternal,
} from "./navigation.js";
export { createNavigation } from "./navigation.js";

export type {
	ThumbnailRail,
	ThumbnailRailOptions,
	ThumbnailRailState,
	ThumbnailView,
	ThumbnailViewport,
} from "./thumbnails.js";
export {
	DEFAULT_THUMBNAIL_GAP,
	MAX_THUMBNAIL_ROWS,
	MAX_THUMBNAIL_WIDTH,
	MIN_THUMBNAIL_WIDTH,
	THUMBNAIL_OVERSCAN_PX,
	createThumbnailRail,
	thumbnailClassName,
} from "./thumbnails.js";

export type {
	NavigationCommand,
	NavigationCommandAction,
	NavigationKey,
	NavigationKeyContext,
} from "./keyboard.js";
export { resolveNavigationKey } from "./keyboard.js";

export type {
	NavigationCatalogue,
	NavigationMessageKey,
	NavigationStrings,
	PageListCatalogue,
	PageListMessageKey,
	PageListMessageValues,
	PageListStrings,
	SearchCatalogue,
	SearchMessageKey,
	SearchStrings,
	ViewerMessageKey,
	ViewerRuntimeOptions,
	ViewerStrings,
} from "./strings.js";
export {
	DEFAULT_NAVIGATION_STRINGS,
	DEFAULT_SEARCH_STRINGS,
	DEFAULT_STRINGS,
	EN_NAVIGATION_CATALOGUE,
	EN_PAGE_LIST_CATALOGUE,
	EN_SEARCH_CATALOGUE,
	EN_VIEWER_CATALOGUE,
	NAVIGATION_MESSAGE_KEYS,
	PAGE_LIST_MESSAGE_KEYS,
	SEARCH_MESSAGE_KEYS,
	VIEWER_MESSAGE_KEYS,
	createNavigationStrings,
	createPageListStrings,
	createPseudoViewerStrings,
	createSearchStrings,
	createViewerMessageRuntime,
	createViewerStrings,
	formatMessage,
	modeKey,
	navigationStrings,
	pageListStrings,
	searchStrings,
} from "./strings.js";
