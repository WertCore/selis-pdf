/**
 * SL-4.UI.08 - print planning.
 *
 * The Do: line is explicit that print must NOT be the browser's own PDF
 * printing: "Render at print resolution to a print-specific canvas or a
 * generated print-ready PDF; honour `/PrintScaling` and page size; do not rely
 * on the browser's own PDF printing." Calling `window.print()` - which is what
 * the shell's Print button does today - hands the decision to a print pipeline
 * we neither control nor test, and cannot meet that Do:.
 *
 * This module is the part of that which is a DECISION rather than a side
 * effect, kept pure so it is testable without a browser or a PDF writer: what
 * `/PrintScaling` means for this page, at what resolution the page has to be
 * rasterised to be "print resolution", and what paper the result is fitted to.
 *
 * PDF assembly consumes this plan; it must not re-derive any of it. That split
 * matters: these are the numbers a wrong print would be judged on, so they need
 * to be checkable on their own.
 */

/** `/PrintScaling` - how the page asks to be scaled onto paper. */
export type PrintScaling = "none" | "appDefault" | "shrinkToFit";

/** A page box in PDF points (1/72 inch), as `/MediaBox` declares it. */
export interface PageBox {
	readonly widthPt: number;
	readonly heightPt: number;
}

/** What the print pipeline will do, and why. */
export interface PrintPlan {
	/** Device pixels per inch to rasterise at. */
	readonly dpi: number;
	/** Scale from PDF points to device pixels. */
	readonly scale: number;
	/**
	 * Size to put on paper, in points, or `null` for the page's own size - which
	 * is the common case and must be the DEFAULT: a viewer that silently
	 * re-papers a document is altering the document.
	 */
	readonly paper: PageBox | null;
	/** Fit-to-page, or print at the document's own size. */
	readonly fitToPage: boolean;
	/** Why this plan, for the record and for the UI to say out loud. */
	readonly reason: string;
}

/** Points per inch. */
export const POINTS_PER_INCH = 72;

/**
 * The minimum DPI this project will call print resolution. Below roughly 240
 * glyph stems and the rasteriser's own antialiasing visibly break up, so a
 * "print resolution" that prints softer than a decent screen is not one.
 */
export const MIN_PRINT_DPI = 300;

/**
 * Parse a raw `/PrintScaling` name.
 *
 * Unknown values fall back to `appDefault` rather than throwing. A malformed
 * entry must not stop the page printing: the spec calls for an unknown value to
 * be ignored, and "ignored" means the viewer's own behaviour applies.
 *
 * @param raw the name as it appears in the page dictionary
 * @returns the parsed scaling
 */
export function parsePrintScaling(raw: string | null | undefined): PrintScaling {
	switch (raw) {
		case "None":
			return "none";
		case "shrinkToFit":
			return "shrinkToFit";
		// `AppDefault` is the PDF 1.7 spelling and `appDefault` is what several
		// real producers write. Both are accepted: a viewer honouring only the
		// spec spelling silently mis-prints documents that spell it the other way.
		case "AppDefault":
		case "appDefault":
			return "appDefault";
		default:
			return "appDefault";
	}
}

/**
 * Decide how to print one page.
 *
 * The three values are genuinely different intentions and are NOT collapsed:
 *
 *  - `none` - the author sized this page for this paper. No fitting, no scaling.
 *  - `shrinkToFit` - the author asks to be fitted onto the chosen paper.
 *    Fitting is allowed; ENLARGING is not, which is why the fit is clamped.
 *  - `appDefault` - the author expressed no preference, so the viewer decides.
 *    This project prints at the document's own size, the conservative choice:
 *    a document printed on paper other than it was designed for is a different
 *    document.
 *
 * @param page the page's own box, in points
 * @param scaling the page's `/PrintScaling`
 * @param paper the selected paper, in points; `null` to use the page's own size
 * @returns the plan
 */
export function planPrint(page: PageBox, scaling: PrintScaling, paper: PageBox | null): PrintPlan {
	// Guarded first. A zero or negative box makes the fit infinite or negative
	// and yields a silently absurd DPI, so it is rejected at the boundary rather
	// than deep in the arithmetic where it would look like a plausible number.
	if (!(page.widthPt > 0) || !(page.heightPt > 0)) {
		throw new RangeError(
			`cannot print a page of ${page.widthPt}x${page.heightPt} points: the box is not positive`,
		);
	}

	const wantsFit = scaling === "shrinkToFit" && paper !== null;
	const target: PageBox = wantsFit && paper !== null ? paper : page;

	// Fit by the SMALLER relative dimension, and never above 1:1. Both matter.
	// Fitting by one axis only stretches a page whose aspect ratio differs from
	// the paper's; scaling above 1 to fill a smaller sheet magnifies a document
	// the author sized down on purpose, and `shrinkToFit` says shrink.
	const fit = Math.min(target.widthPt / page.widthPt, target.heightPt / page.heightPt);
	const scale = wantsFit && fit < 1 ? fit : 1;

	// The floor is applied by scaling UP to reach it, never by honouring a lower
	// request: at 150 DPI the text is already too soft, and the fix for that is
	// more samples, not fewer.
	const dpi = MIN_PRINT_DPI;

	return {
		dpi,
		scale: (scale * dpi) / POINTS_PER_INCH,
		paper: wantsFit && paper !== null ? paper : null,
		fitToPage: scale < 1,
		reason: wantsFit
			? scale < 1
				? `/PrintScaling /shrinkToFit: fitted to ${paper?.widthPt}x${paper?.heightPt}pt`
				: "/PrintScaling /shrinkToFit: the page already fits, printed at its own size"
			: scaling === "none"
				? "/PrintScaling /None: printed at the page's own size, unfitted"
				: "/PrintScaling /AppDefault: the document expressed no preference, so it is printed at its own size",
	};
}

/**
 * The most pages one print job will assemble.
 *
 * This is a memory bound, not a taste judgement, and it is stated here rather
 * than discovered in a tab. `print-pdf.ts` writes image data UNCOMPRESSED, so
 * one 300 DPI Letter page contributes 2550x3300x3 = 25,245,000 bytes to the
 * finished file, and the file is accumulated in memory until the Blob is made.
 * Forty pages is therefore about 1 GB of raster held at once.
 *
 * A document with more pages than this is REFUSED, with its real page count
 * named, rather than truncated. Printing pages 1-40 of 400 and saying nothing
 * is the failure mode this bound exists to prevent: a user who is handed a
 * 40-page PDF has been given a different document and no way to tell.
 *
 * The bound is lifted by the obvious next step recorded in `print-pdf.ts`: a
 * `/FlateDecode` image stream cuts it by roughly an order of magnitude.
 */
export const MAX_PRINT_PAGES = 40;

/**
 * The printed box for one page, and the scale that produced it.
 *
 * `rotated` is not decoration. See {@link reconcilePageBox}.
 */
export interface PrintedPage {
	/** The box to put on paper, in points, rotation already applied. */
	readonly box: PageBox;
	/** Device pixels per PDF point, MEASURED from the raster. */
	readonly scale: number;
	/** Whether the raster came back transposed against the declared box. */
	readonly rotated: boolean;
}

/** One page box as the engine's `open` reply reports it. */
interface WirePageSize {
	readonly width: number;
	readonly height: number;
}

/**
 * The page count and every page box, from the engine's `open` reply.
 *
 * The `open` op answers `{doc, pages, pageSizes}` - `pages` is
 * `session.len()` and `pageSizes` is one entry per page, index-aligned. That
 * makes a WALK unnecessary, and the walk is the thing that went wrong: probing
 * `{op:"page"}` for N+1 to discover where the document ends costs N+1 round
 * trips through a 25 MB-per-page render path, and it derives the count from an
 * ERROR rather than from a number the engine already computed.
 *
 * (`op_page` does in fact refuse out of range with `PAGE_OUT_OF_RANGE` -
 * `crates/selis-pdf-wasm/src/lib.rs` pins it - so the walk would have
 * terminated. It is still the wrong instrument, and this records why.)
 *
 * Every field is validated rather than trusted. A `pageSizes` array shorter
 * than `pages` would put the wrong MediaBox on the wrong page, and a silent
 * truncation to the shorter length is exactly how that ships.
 *
 * @param open the `open` reply's `value`
 * @returns the count and one box per page, in document order
 */
export function printBoxesFromOpen(open: unknown): { count: number; boxes: PageBox[] } {
	if (open === null || typeof open !== "object") {
		throw new RangeError("the open reply carried no value to read pages from");
	}
	const value = open as { pages?: unknown; pageSizes?: unknown };
	const { pages } = value;
	if (typeof pages !== "number" || !Number.isInteger(pages) || pages < 0) {
		throw new RangeError(`the open reply has no usable page count: ${String(pages)}`);
	}
	const sizes = value.pageSizes;
	if (!Array.isArray(sizes) || sizes.length !== pages) {
		throw new RangeError(
			`the open reply says ${pages} page(s) but carries ${Array.isArray(sizes) ? sizes.length : "no"} \
page size(s); the two must be index-aligned or the boxes land on the wrong pages`,
		);
	}
	return { count: pages, boxes: sizes.map((size, index) => toBox(size, index)) };
}

/** Validate one wire page size into a {@link PageBox}. */
function toBox(size: unknown, index: number): PageBox {
	const { width, height } = (size ?? {}) as Partial<WirePageSize>;
	if (
		typeof width !== "number" ||
		typeof height !== "number" ||
		!Number.isFinite(width) ||
		!Number.isFinite(height) ||
		width <= 0 ||
		height <= 0
	) {
		throw new RangeError(
			`page ${index} has no usable box (${String(width)}x${String(height)}); a page with no \
positive MediaBox cannot be given a MediaBox of its own`,
		);
	}
	return { widthPt: width, heightPt: height };
}
/**
 * Reconcile a page's DECLARED box against the raster the engine actually
 * produced, and return the box to print.
 *
 * ## Why this is not optional
 *
 * The engine reports two different shapes for the same page, and they are not
 * the same shape for a rotated page:
 *
 *  - `open`/`page` report `/MediaBox` **un-rotated** - the page's own user
 *    space, 612x792 for a portrait scan.
 *  - `render` goes through `Session::page_view`, which applies `/Rotate`. A
 *    `/Rotate 90` scan of that same page comes back 792x612.
 *
 * Writing the un-rotated box beside a rotated raster is a page printed
 * SQUASHED: the writer scales the image to fill the MediaBox it was given, so a
 * landscape scan lands on portrait paper, distorted. `/Rotate` is inherited
 * and appears on real documents constantly - every landscape page produced by
 * a phone or a scanner carries it - so this is the common case, not the exotic
 * one. The Do: for UI.08 asks for the page SIZE to be honoured, and this is
 * where honouring it is won or lost.
 *
 * The engine does not expose `/Rotate` on the protocol, so the orientation is
 * recovered from the raster itself: a uniform scale means both axes agree, and
 * only one pairing of the two boxes can make them agree. A square page matches
 * both pairings and needs no correction either way, which is why the ambiguity
 * there is harmless.
 *
 * `scale` is measured, not requested. Returning it is the point: a shell that
 * reported the DPI it ASKED for would report 300 for a raster the engine
 * quietly rendered at 72, which is a real defect that passed a green check.
 *
 * @param declared the box the engine reported for this page, in points
 * @param rasterWidthPx the width of the raster the engine returned
 * @param rasterHeightPx the height of that raster
 * @returns the box to print, the measured scale, and whether it was transposed
 */
export function reconcilePageBox(
	declared: PageBox,
	rasterWidthPx: number,
	rasterHeightPx: number,
): PrintedPage {
	if (!(declared.widthPt > 0) || !(declared.heightPt > 0)) {
		throw new RangeError(
			`cannot print a page of ${declared.widthPt}x${declared.heightPt} points: the box is not positive`,
		);
	}
	if (
		!Number.isInteger(rasterWidthPx) ||
		!Number.isInteger(rasterHeightPx) ||
		rasterWidthPx <= 0 ||
		rasterHeightPx <= 0
	) {
		throw new RangeError(
			`the engine returned a ${rasterWidthPx}x${rasterHeightPx} raster, which cannot be measured \
against a page box`,
		);
	}

	// Candidate 1: the raster is upright, so its width pairs with the box's
	// width. Candidate 2: the page is turned a quarter turn, so the raster's
	// width pairs with the box's HEIGHT.
	const upright = rasterWidthPx / declared.widthPt;
	const turned = rasterWidthPx / declared.heightPt;

	// One pixel of slack: the engine rounds the canvas to whole pixels, so a
	// perfectly upright page misses its own height by a fraction of a pixel.
	if (Math.abs(rasterHeightPx - declared.heightPt * upright) <= 1) {
		return { box: declared, scale: upright, rotated: false };
	}
	if (Math.abs(rasterHeightPx - declared.widthPt * turned) <= 1) {
		return {
			box: { widthPt: declared.heightPt, heightPt: declared.widthPt },
			scale: turned,
			rotated: true,
		};
	}
	// Neither pairing holds. Rather than pick one, report the declared box and
	// let the measured scale stand: the DPI reading is then visibly wrong (it
	// will not be near 300), which is the honest outcome for a raster whose
	// shape agrees with neither box. Guessing here would produce a plausible
	// number for a broken render.
	return { box: declared, scale: upright, rotated: false };
}

/**
 * The DPI a raster actually carries, measured rather than claimed.
 *
 * `round(rasterWidthPx / box.widthPt * 72)`, against the box the page is
 * PRINTED at - not the box the engine declared, which for a rotated page is the
 * transposed one. Reporting the requested DPI here instead is a real defect
 * that passed a green check once: a 72 DPI render reported as 300.
 *
 * @param rasterWidthPx the raster's width in device pixels
 * @param box the box the page is printed at, in points
 * @returns the measured DPI
 */
export function measuredPrintDpi(rasterWidthPx: number, box: PageBox): number {
	if (!(box.widthPt > 0)) {
		throw new RangeError(`cannot measure DPI against a ${box.widthPt}pt-wide page`);
	}
	return Math.round((rasterWidthPx / box.widthPt) * POINTS_PER_INCH);
}

/**
 * WHY THIS IS SEPARATE FROM RENDERING - read before implementing the raster.
 *
 * `selis_render_page(pdf, len, page, &w, &h, &len)` takes **no scale**. It
 * always renders one point to one pixel, which is why every page this project
 * has measured comes back at exactly its `/MediaBox` size (612x792). There is no
 * way to ask it for 300 DPI, and no argument to add: ADR-P0041 freezes that raw
 * ABI as the perf harness's measured surface, so widening it is closed by
 * design rather than by oversight.
 *
 * The only route to a print-resolution raster is the JSON protocol's `Render`
 * op, which takes `RenderParams` - the same "canvas and tile selection" the
 * tile path uses (ADR-P0011). The shell's `boot.js` currently speaks ONLY the
 * raw ABI, so print is also the first thing that will need it to speak the
 * message protocol for rendering as well as for search.
 *
 * Two consequences worth stating before anyone builds on this:
 *
 *  - A Letter page at 300 DPI is 2550x3300, which is 33 MB of RGBA for ONE
 *    page. A whole document printed that way does not fit in a tab, so the
 *    print path has to be page-at-a-time and streaming into the PDF writer -
 *    it cannot "render everything then assemble".
 *  - The plan's `scale` is expressed in points-to-device, so it composes with
 *    the `Render` matrix directly: matrix scale = `plan.scale / (dpi / 72)` for
 *    the fit, and the device raster is `plan.scale` overall.
 *
 * CONFIRMED: the JSON protocol's `Render` op carries `RenderParams`, whose
 * first field is `dpi: f64`, documented as "render resolution in dots per inch
 * (72 = 1 pt per px)". The engine can therefore already rasterise at print
 * resolution, deriving the page-to-device matrix from `dpi` while honouring
 * `/Rotate` - which matters for print, since a rotated page has to come out
 * rotated or a landscape scan prints portrait.
 *
 * The join between this module and the engine, exactly:
 *
 *     RenderParams { dpi: plan.dpi }   ->  points-to-device scale
 *     plan.scale / (plan.dpi / 72)     ->  the FIT, if any
 *     (box.widthPt, box.heightPt)      ->  the MediaBox the writer writes
 *
 * `plan.dpi` is therefore not advisory: it is the number the engine is asked for,
 * and `buildPrintPdf` writes the same boxes it was derived from. If a rasteriser
 * ever takes its DPI from anywhere else, the printed page is the wrong physical
 * size and nothing downstream notices - the MediaBox and the pixels disagree
 * quietly, which is the most expensive kind of print bug.
 *
 * `RenderParams` also has an optional `tile`. Not needed yet, but it is the
 * escape hatch for large pages: a 300 DPI Letter page is 33 MB of RGBA, and a
 * tile could be rendered and freed a strip at a time rather than all at once.
 * Worth knowing the option exists before designing around a bigger buffer.
 */
