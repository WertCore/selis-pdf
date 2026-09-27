/**
 * Scroll anchoring across relayout (SL-4.UI.02, "correct scroll anchoring on
 * zoom").
 *
 * The failure this exists to prevent: the reader is halfway down page 900,
 * presses +, and the document jumps somewhere else. The fix is to name the
 * anchor *before* the relayout as (page, fraction-of-page-above-the-fold) and
 * to solve for the scroll offset that reproduces that same view afterwards.
 *
 * Anchoring on a page + fraction rather than a raw pixel offset is deliberate:
 * a raw offset is only correct if everything above it scales uniformly, which
 * a mode change (single → spread re-pairs every row) or a viewport resize does
 * not guarantee. The page is a document-level identity; the fraction is the
 * reader's position within it. Both survive any relayout that keeps the page
 * list a list.
 */

import type { PageLayout } from "./layout.js";
import { pageAtScrollTop } from "./layout.js";
import { clampScrollTop } from "./windowing.js";

/** A document-relative description of "what the reader is looking at". */
export interface ScrollAnchor {
	/** Page whose top edge (or the part of it above the fold) was at the top. */
	readonly page: number;
	/** 0..1 through the anchor page — how far the reader had scrolled into it. */
	readonly fraction: number;
	/** Pixel offset from the page's top edge to the top of the viewport. */
	readonly offsetPx: number;
}

function clamp01(value: number): number {
	return Math.min(1, Math.max(0, Number.isFinite(value) ? value : 0));
}

/** Capture the anchor for the current scroll offset. `null` for an empty document. */
export function captureAnchor(layout: PageLayout, scrollTop: number): ScrollAnchor | null {
	const entry = pageAtScrollTop(layout, scrollTop);
	if (entry === null) {
		return null;
	}
	const offsetPx = scrollTop - entry.y;
	return {
		page: entry.page,
		fraction: clamp01(offsetPx / Math.max(1, entry.height)),
		offsetPx,
	};
}

/**
 * Scroll offset that puts the same part of the same page at the top of the
 * viewport after a relayout. Returns `null` when the anchor page is gone (a
 * different document), so the caller can fall back to offset preservation.
 */
export function anchorScrollTop(
	anchor: ScrollAnchor | null,
	layout: PageLayout,
	viewportHeight: number,
): number | null {
	if (anchor === null) {
		return null;
	}
	const entry = layout.pages.find((candidate) => candidate.page === anchor.page);
	if (entry === undefined) {
		return null;
	}
	// Solve on the fraction, not the pixel offset: after a zoom the page is a
	// different height, and "the same third of the way in" is what the reader
	// means by "where I was".
	return clampScrollTop(layout, entry.y + anchor.fraction * entry.height, viewportHeight);
}

/**
 * Fallback when the anchor page cannot be resolved: keep the same *distance
 * into the document* by ratio, clamped. Used for zoom on an empty or
 * not-yet-laid-out document, where there is nothing better to preserve.
 */
export function proportionalScrollTop(
	previous: PageLayout,
	next: PageLayout,
	scrollTop: number,
	viewportHeight: number,
): number {
	const ratio =
		previous.contentHeight <= 0 ? 0 : Math.min(1, Math.max(0, scrollTop / previous.contentHeight));
	return clampScrollTop(next, ratio * next.contentHeight, viewportHeight);
}
