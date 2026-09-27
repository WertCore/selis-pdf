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
	tileKey,
} from "./tile-ladder.js";

export type { KeyboardContext, PageListKey, PageNavigationCommand } from "./keyboard.js";
export { announcement, pageLabel, resolvePageKey, stepFor } from "./keyboard.js";

export type {
	PageList,
	PageListMetrics,
	PageListOptions,
	PageListState,
	PageListViewport,
	PageTileView,
} from "./page-list.js";
export { DEFAULT_METRICS, createPageList } from "./page-list.js";
