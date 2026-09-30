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
export function parsePrintScaling(raw) {
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
export function planPrint(page, scaling, paper) {
    // Guarded first. A zero or negative box makes the fit infinite or negative
    // and yields a silently absurd DPI, so it is rejected at the boundary rather
    // than deep in the arithmetic where it would look like a plausible number.
    if (!(page.widthPt > 0) || !(page.heightPt > 0)) {
        throw new RangeError(`cannot print a page of ${page.widthPt}x${page.heightPt} points: the box is not positive`);
    }
    const wantsFit = scaling === "shrinkToFit" && paper !== null;
    const target = wantsFit && paper !== null ? paper : page;
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
//# sourceMappingURL=print.js.map