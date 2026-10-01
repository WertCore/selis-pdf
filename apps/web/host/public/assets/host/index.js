/**
 * SL-4.UI.08 - the host's page-side entry point.
 *
 * `apps/web/host/src/index.ts` is the service worker's source generator and has
 * nothing to do with the page. This module exists so `place-assets.mjs` has a
 * page entry to walk from: because that walk follows IMPORTS, re-exporting only
 * what the page needs ships exactly those modules and nothing else. Adding the
 * host's `dist` with `index.js` as the entry would have put the worker - sw,
 * headers, no-upload, worker-glue - on the page origin for no reason.
 *
 * Anything the shipped page imports must be listed here. That is a deliberate
 * cost: an unlisted module does not ship, so "works locally, 404s deployed" is
 * a build-time failure rather than a runtime one.
 */
export { MAX_PRINT_PAGES, MIN_PRINT_DPI, POINTS_PER_INCH, measuredPrintDpi, parsePrintScaling, planPrint, printBoxesFromOpen, reconcilePageBox, } from "./print.js";
export { buildPrintPdf, rgbaToRgb, streamPrintPdf, } from "./print-pdf.js";
//# sourceMappingURL=page.js.map