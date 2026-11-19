/**
 * The compare card's presentation helpers.
 *
 * DOM-free and side-effect free, so the option copy and the result rows can be
 * tested without a browser. Op 46 returns a `selis-compare/1` JSON report as a
 * downloadable output; the engine's own booleans and fractions drive these rows
 * rather than any claim copied from the request.
 */

import type { VerificationRow } from "./protocol.js";

/** One page position in the engine's comparison report. */
export interface PageComparisonJson {
	readonly page?: number;
	readonly present_in?: string;
	readonly pixels_identical?: boolean;
	readonly diff_fraction?: number;
}

/** The shape of the `selis-compare/1` JSON the chunk writes. */
export interface ComparisonReportJson {
	readonly schema?: string;
	readonly status?: string;
	readonly pages_a?: number;
	readonly pages_b?: number;
	readonly identical?: boolean;
	readonly text_identical?: boolean;
	readonly pages?: readonly PageComparisonJson[];
}

/** The schema tag the engine stamps on every comparison report. */
export const COMPARE_SCHEMA = "selis-compare/1";

/** The honest option note: what is compared, at what resolution, and locally. */
export const COMPARE_NOTE =
	"Both documents are rendered page by page through the same engine at the same resolution, then compared sample for sample — the verdict is deterministic. The extracted text is compared across each whole document alongside the pixels. A page present in only one file, or drawn at different dimensions, is reported as different. Your files are read here and never uploaded.";

/** Cap the per-page rows so a hostile or enormous report cannot grow the card. */
const MAX_PAGE_ROWS = 8;

/** A bounded fraction as a whole percentage, or "?" when the engine omitted it. */
function percent(fraction: number | undefined): string {
	if (typeof fraction !== "number" || !Number.isFinite(fraction)) return "?";
	const clamped = Math.max(0, Math.min(1, fraction));
	return `${Math.round(clamped * 100)}%`;
}

/**
 * The rows the compare card adds beside the standard verification rows.
 *
 * The headline verdict, the text verdict and the page counts come straight
 * from the engine's report; one row per differing page follows, capped so a
 * long document stays readable. Values are never taken from the request.
 */
export function compareReportRows(report: ComparisonReportJson): VerificationRow[] {
	if (report.schema !== COMPARE_SCHEMA) {
		return [{ label: "Compare", value: "the report is not a selis-compare report" }];
	}
	const rows: VerificationRow[] = [
		{
			label: "Result",
			value:
				report.identical === true
					? "identical — the same pixels and the same text"
					: report.status === "ok"
						? "different"
						: "the comparison did not complete",
		},
		{
			label: "Text",
			value:
				report.text_identical === true
					? "the extracted text matches"
					: "the extracted text differs",
		},
		{
			label: "Pages",
			value: `${report.pages_a ?? "?"} vs ${report.pages_b ?? "?"}`,
		},
	];

	const pages = report.pages ?? [];
	const differing = pages.filter(
		(page) =>
			page.present_in !== "both" || (page.present_in === "both" && page.pixels_identical !== true),
	);
	for (const page of differing.slice(0, MAX_PAGE_ROWS)) {
		const label = `Page ${page.page ?? "?"}`;
		const value =
			page.present_in === "a_only"
				? "only in the first document"
				: page.present_in === "b_only"
					? "only in the second document"
					: `${percent(page.diff_fraction)} of pixels differ`;
		rows.push({ label, value });
	}
	if (differing.length > MAX_PAGE_ROWS) {
		rows.push({
			label: "More differences",
			value: `${differing.length - MAX_PAGE_ROWS} further page(s) — see the downloaded report`,
		});
	}
	return rows;
}
