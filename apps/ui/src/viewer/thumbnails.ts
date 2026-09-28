/**
 * The thumbnail rail (SL-4.UI.06): a vertical strip of page previews.
 *
 * ## It is the page list's ladder, at a fixed width — and nothing else
 *
 * The viewer README says this and it is the load-bearing sentence of this file:
 * a thumbnail is *the same rasterisation* as a page tile, at a smaller scale,
 * with the same placeholder → low-res → full rung structure, the same bounded
 * concurrency, the same cancellation and the same LRU cache. So this module does
 * not render anything: it calls {@link planLadder} with a `cssScale` derived
 * from the rail's width and hands the tasks to a {@link TileScheduler} of its
 * own, and the shell paints the resulting `TileEntry` exactly as UI.03's
 * compositor paints a page tile. A second rasterisation path here would be a
 * second set of bugs — and the most likely bug of all, a thumbnail rail that
 * looks sharper than the page it is a preview of, because it bypassed the
 * ladder's rung policy.
 *
 * Two scheduler instances, not one: the rail and the page list have independent
 * caches, concurrency budgets and cancellations, because scrolling the document
 * must not evict the previews the reader is looking at. Two *instances* of one
 * class is not a second implementation; sharing one cache would also be correct
 * (the ladder keys on `page:stage@scale`) but would couple the two surfaces'
 * memory, and a rail at a fixed width is by construction a scale the page list
 * never asks for.
 *
 * ## What the rail knows and what it does not
 *
 * It knows the rail's width, height, scroll offset and the current page; all
 * four are host facts the shell measures. It does not know the document area's
 * geometry, the zoom, or whether a tile has been painted — a rail thumbnail at
 * 120 px is the same bitmap whatever the document is zoomed to, which is why the
 * ladder keys on the rail's own scale and why a zoom repaints nothing here.
 */

import type { PlatformAdapter } from "../platform/adapter.js";
import type { AdapterError } from "../platform/errors.js";
import type { DocHandle, PageLabelRange, Size } from "../platform/types.js";
import { type NormalisedLabelRange, normaliseLabelRanges, pageLabelFor } from "./page-labels.js";
import type { NavigationCatalogue, PageListCatalogue } from "./strings.js";
import { createNavigationStrings } from "./strings.js";
import type { TileEntry, TileStage, TileTask } from "./tile-ladder.js";
import { TileScheduler, planLadder } from "./tile-ladder.js";

/** What the shell measures and hands in. All host facts, none guessed. */
export interface ThumbnailViewport {
	/** Rail width in CSS pixels. This is what fixes every thumbnail's scale. */
	readonly width: number;
	/** Rail height in CSS pixels. */
	readonly height: number;
	/** Rail scroll offset in CSS pixels. */
	readonly scrollTop: number;
	/** Device pixels per CSS pixel. */
	readonly devicePixelRatio: number;
}


/** One thumbnail to paint. */
export interface ThumbnailView {
	readonly page: number;
	/** The document's own label for the page ("iv", "A-7", "12"). */
	readonly label: string;
	/** Rail-space box, CSS pixels. Identical at every rung of the ladder. */
	readonly y: number;
	readonly width: number;
	readonly height: number;
	/** Which rung is available now. */
	readonly stage: TileStage;
	/** Device pixels per PDF point the current rung was rasterised at. */
	readonly deviceScale: number;
	/** The cached tile, or `null` before one lands. The shell paints this. */
	readonly tile: TileEntry | null;
	/** `.selis-*` class list; never a hard-coded colour or size. */
	readonly className: string;
	/** Roving tabindex: the current page is the one in the tab order. */
	readonly tabIndex: 0 | -1;
	readonly current: boolean;
	/** `aria-label`; from the catalogue, never a literal. */
	readonly action: string;
}

/** Everything the shell paints for the rail, in one immutable value. */
export interface ThumbnailRailState {
	readonly pageCount: number;
	readonly currentPage: number;
	readonly scrollTop: number;
	/** Rail content height, so the shell can size the scroller. */
	readonly contentHeight: number;
	readonly thumbnails: readonly ThumbnailView[];
	/** Polite live-region text. */
	readonly announcement: string;
	/** `aria-label` for the rail. */
	readonly railLabel: string;
	/** True when the rail's own row cap stopped the window. */
	readonly windowClamped: boolean;
}

/**
 * Bounds on the rail's width.
 *
 * Both ends are honest limits rather than preferences. Below the minimum a
 * thumbnail is not a preview of anything; above the maximum it is a second
 * document view competing with the first for the engine's budget, at which point
 * the reader should widen the window rather than get a second viewport.
 */
export const MIN_THUMBNAIL_WIDTH = 32;
export const MAX_THUMBNAIL_WIDTH = 320;

/**
 * Thumbnails rendered beyond this window are not rendered at all.
 *
 * A 2 000-page document's rail is 2 000 rows; a rail that rasterised all of them
 * would ask the engine for 2 000 renders to fill a strip showing six. The
 * overscan is in pixels for the reason `windowing.ts` uses a pixel band: a
 * document of small thumbnails and a document of tall ones want the same *time*
 * of runway, and a row count would give them different amounts.
 */
export const MAX_THUMBNAIL_ROWS = 64;
export const THUMBNAIL_OVERSCAN_PX = 200;

/** Default gap between thumbnails, in CSS pixels. A density token's job. */
export const DEFAULT_THUMBNAIL_GAP = 8;

export interface ThumbnailRailOptions {
	readonly adapter: PlatformAdapter;
	readonly doc: DocHandle;
	/** Rail width in CSS pixels; the default is a desktop-sized rail. */
	readonly width?: number;
	/** Vertical gap between thumbnails, in CSS pixels. */
	readonly gap?: number;
	/** Engine tasks submitted per frame. */
	readonly frameBudget?: number;
	/** The document's `/PageLabels` ranges, if it has any. */
	readonly pageLabels?: readonly PageLabelRange[];
	/** Navigation catalogue override, merged over English. */
	readonly strings?: Partial<NavigationCatalogue>;
	/** Page-list catalogue, used only to word a page in the live region. */
	readonly pageStrings?: Partial<PageListCatalogue>;
	readonly onState?: (state: ThumbnailRailState) => void;
	readonly onError?: (error: AdapterError) => void;
}

/** The controller the shell programs against. */
export interface ThumbnailRail {
	/** Report the rail's metrics; returns the republished state. */
	update(viewport: ThumbnailViewport): ThumbnailRailState;
	/** The last state. */
	state(): ThumbnailRailState;
	/** Report which page the document is showing, for `aria-current`. */
	setCurrentPage(page: number): ThumbnailRailState;
	/** Tell the rail the document's own page labels (UI.06's page labels). */
	setPageLabels(ranges: readonly PageLabelRange[]): ThumbnailRailState;
	/** The cached tile for a page, for a shell painting one directly. */
	tileAt(page: number): TileEntry | null;
	subscribe(listener: (state: ThumbnailRailState) => void): () => void;
	/** Abort in-flight renders and release the cache. */
	dispose(): void;
}

/** One row of the rail: a page, its box, and the scale its preview is drawn at. */
interface RailRow {
	readonly page: number;
	readonly y: number;
	readonly height: number;
	/** CSS pixels per PDF point at the rail's width. */
	readonly cssScale: number;
}

/**
 * The rail's geometry: one box per page, in rail coordinates.
 *
 * Derived from the document's own page sizes and the rail's width, exactly as
 * `layout.ts` derives the page list's boxes from the viewport — same rule, same
 * reason: the box is a function of the document and the rail, so a preview
 * resolving cannot move it. A thumbnail that grew a pixel when its bitmap
 * arrived would be UI.02's layout-shift DoD in a smaller costume.
 */
function railRows(pageSizes: readonly Size[], width: number, gap: number): RailRow[] {
	const usable = Math.max(1, Math.min(MAX_THUMBNAIL_WIDTH, width) - gap);
	return pageSizes.map((size, page) => {
		const ratio = size.height > 0 && size.width > 0 ? size.height / size.width : 1;
		return {
			page,
			y: page * (usable * ratio + gap),
			height: usable * ratio,
			cssScale: usable / (size.width > 0 ? size.width : 1),
		};
	});
}

/** Create the thumbnail rail. */
export function createThumbnailRail(options: ThumbnailRailOptions): ThumbnailRail {
	const doc = options.doc;
	const strings = createNavigationStrings(options.strings);
	const gap = Math.max(0, options.gap ?? DEFAULT_THUMBNAIL_GAP);
	const scheduler = new TileScheduler({
		engine: options.adapter.engine,
		doc,
		// A tile landing republishes the state, so the shell cannot forget to
		// forward it and leave the rail stuck on placeholders.
		onTile: () => {
			publish();
		},
		...(options.onError === undefined ? {} : { onError: options.onError }),
	});
	let viewport: ThumbnailViewport = {
		width: options.width ?? MAX_THUMBNAIL_WIDTH / 2,
		height: 0,
		scrollTop: 0,
		devicePixelRatio: 1,
	};
	let currentPage = 0;
	let rows: readonly RailRow[] = railRows(doc.pageSizes, viewport.width, gap);
	let labels: readonly NormalisedLabelRange[] = normaliseLabelRanges(options.pageLabels);
	let lastState: ThumbnailRailState | null = null;
	const subscribers = new Set<(state: ThumbnailRailState) => void>();

	function relayout(): void {
		rows = railRows(doc.pageSizes, viewport.width, gap);
	}

	/** The page whose row is at or above `y`, by binary search over the pitch. */
	function rowAt(y: number): number {
		if (rows.length === 0) {
			return 0;
		}
		let low = 0;
		let high = rows.length - 1;
		while (low < high) {
			const mid = (low + high + 1) >> 1;
			const row = rows[mid];
			if (row !== undefined && row.y <= y) {
				low = mid;
			} else {
				high = mid - 1;
			}
		}
		return low;
	}

	function windowFor(next: ThumbnailViewport): {
		visible: readonly number[];
		overscan: readonly number[];
		clamped: boolean;
	} {
		const top = Math.max(0, next.scrollTop) - THUMBNAIL_OVERSCAN_PX;
		const bottom = Math.max(0, next.scrollTop) + Math.max(0, next.height) + THUMBNAIL_OVERSCAN_PX;
		const first = rowAt(top);
		const last = rowAt(bottom);
		const end = Math.min(rows.length - 1, Math.max(first, last));
		const clamped = end - first + 1 > MAX_THUMBNAIL_ROWS;
		const stop = clamped ? first + MAX_THUMBNAIL_ROWS - 1 : end;
		const visible: number[] = [];
		const overscan: number[] = [];
		for (let page = first; page <= stop; page += 1) {
			visible.push(page);
		}
		for (let page = stop + 1; page <= end; page += 1) {
			overscan.push(page);
		}
		return { visible, overscan, clamped };
	}

	/**
	 * The ladder call — the reason this file is small.
	 *
	 * `planLadder` takes one `cssScale` for the whole window, which is right for
	 * the page list (one scale, one zoom) and *almost* right here: a rail
	 * thumbnail's width is the same for every page, but a landscape page needs a
	 * smaller scale to fit that width than a portrait one. So the rail calls the
	 * ladder once per page, with that page's own scale, and concatenates. The
	 * priority order, the rung structure, the satisfied-key logic and the cache
	 * key all stay the ladder's — which is the entire point.
	 *
	 * One task per visible page per frame: the rail is a preview, and a budget
	 * larger than the visible count would prefetch pages the reader may never
	 * scroll to, at the cost of the previews they can see.
	 */
	function tasksFor(next: ThumbnailViewport, satisfied: ReadonlySet<string>): readonly TileTask[] {
		const { visible, overscan } = windowFor(next);
		const plan = (page: number, budget: number): readonly TileTask[] => {
			const row = rows[page];
			if (row === undefined) {
				return [];
			}
			return planLadder({
				doc,
				windowPages: [{ page }],
				visiblePages: new Set([page]),
				devicePixelRatio: next.devicePixelRatio,
				cssScale: row.cssScale,
				satisfied,
				budget,
			});
		};
		return [
			...visible.flatMap((page) => plan(page, 1)),
			// Prefetch outside the rail: the low-res rung only, because a page
			// just off the rail is a preview of itself, not a page.
			...overscan.flatMap((page) => plan(page, 1).filter((task) => task.stage === "lowres")),
		];
	}

	function buildState(): ThumbnailRailState {
		const { visible, clamped } = windowFor(viewport);
		const usable = Math.max(MIN_THUMBNAIL_WIDTH, viewport.width - gap);
		const thumbnails: ThumbnailView[] = visible.map((page) => {
			const row = rows[page] ?? { page, y: 0, height: 0, cssScale: 1 };
			const entry = scheduler.bestFor(page);
			const label = pageLabelFor(page, undefined, labels);
			return {
				page,
				label,
				y: row.y,
				width: usable,
				height: row.height,
				stage: entry?.stage ?? "placeholder",
				deviceScale: entry?.deviceScale ?? 0,
				tile: entry,
				className: thumbnailClassName(entry?.stage ?? "placeholder", page === currentPage),
				tabIndex: page === currentPage ? 0 : -1,
				current: page === currentPage,
				action: strings.thumbnailItem(label),
			};
		});
		const last = rows[rows.length - 1];
		return {
			pageCount: doc.pageCount,
			currentPage,
			scrollTop: Math.max(0, viewport.scrollTop),
			contentHeight: last === undefined ? 0 : last.y + last.height + gap,
			thumbnails,
			announcement: strings.thumbnailItem(pageLabelFor(currentPage, undefined, labels)),
			railLabel: strings.thumbnailsLabel,
			windowClamped: clamped,
		};
	}

	function publish(): ThumbnailRailState {
		lastState = buildState();
		for (const listener of subscribers) {
			listener(lastState);
		}
		return lastState;
	}

	const rail: ThumbnailRail = {
		update(next) {
			if (next.width !== viewport.width) {
				viewport = { ...next };
				relayout();
			} else {
				viewport = next;
			}
			scheduler.submit(tasksFor(viewport, scheduler.satisfiedKeys()));
			return publish();
		},

		state() {
			return lastState ?? buildState();
		},

		setCurrentPage(page) {
			const next = Number.isFinite(page)
				? Math.min(doc.pageCount - 1, Math.max(0, Math.trunc(page)))
				: 0;
			if (next === currentPage) {
				return lastState ?? buildState();
			}
			currentPage = next;
			return publish();
		},

		setPageLabels(next) {
			labels = normaliseLabelRanges(next);
			return publish();
		},

		tileAt(page) {
			return scheduler.bestFor(page);
		},

		subscribe(listener) {
			subscribers.add(listener);
			return () => {
				subscribers.delete(listener);
			};
		},

		dispose() {
			scheduler.dispose();
			subscribers.clear();
		},
	};

	// Publish the empty initial state so a shell can paint before the first
	// metrics report; no engine work is scheduled from it.
	publish();
	return rail;
}

/**
 * The `.selis-*` class list for one thumbnail.
 *
 * Exported because `navigation.css.test.ts` asserts that every class a
 * controller can emit is actually styled — the same gate `page-list.css.test.ts`
 * runs, and the reason the class list is built here and not in a shell.
 */
export function thumbnailClassName(stage: TileStage, current: boolean): string {
	return [
		"selis-thumbnail",
		`selis-thumbnail--${stage}`,
		current ? "selis-thumbnail--current" : "selis-thumbnail--idle",
	].join(" ");
}

