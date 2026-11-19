/**
 * The Redact patterns card's presentation helpers — DOM-free and side-effect
 * free, so the review model and the result rows can be tested without a
 * browser. Mirrors `flatten-forms.ts` / `subset.ts`.
 *
 * The review is the whole point of this card. The guest returns MASKED text
 * only: this module never receives a full secret, and neither the rows it
 * builds nor the JSON it parses can conjure one back. Every row is created
 * unchecked — a caller has to supply an explicit selection before any id can
 * become an approval. There is no scan-and-apply helper here by design.
 */

import {
	type ToolResult,
	type Verification,
	type VerificationRow,
	formatBytes,
} from "./protocol.js";

/** The honest option note: two phases, mandatory review, local-only. */
export const REDACT_PATTERNS_NOTE =
	"Scan first, review, then apply — nothing is redacted until you tick the candidates you approve and press Apply. The review shows masked text only (the full match never leaves this device in the report). The scan has no output file; the apply writes one only for the ids you selected, re-derives every region from the original PDF, and refuses the whole run if a redacted match survives the read-back. Your file is read and written on this device only — it is never uploaded.";

/** Shown above the review table so a reader knows what the rows mean. */
export const REDACT_REVIEW_NOTE =
	"Each row is a possible match, shown masked so the report itself never carries the secret. Tick only the ones you want redacted. An unticked row is left in the document.";

/** The honest limit of the low-level redactor, shown on the result card. */
export const REDACT_VERIFY_NOTE =
	"A match that shares one show operation with surrounding text cannot be excised from that operation. When that happens the read-back fails loudly with VERIFY_FAILED and no file is written or offered — never a document that only looks redacted.";

/** The secret-free pattern label a candidate carries. */
export type PatternKindLabel = "ssn" | "card" | "email" | "literal";

/** One candidate exactly as the guest's masked review serialises it. */
export interface PatternCandidateJson {
	readonly id: string;
	readonly page: number;
	readonly line: number;
	readonly start: number;
	readonly end: number;
	readonly pattern: PatternKindLabel;
	readonly masked: string;
	readonly region: readonly [number, number, number, number];
}

/** The masked review JSON returned by the scan phase. */
export interface PatternReviewJson {
	readonly schema: number;
	readonly source: string;
	readonly source_len: number;
	readonly source_fnv: number;
	readonly candidates: readonly PatternCandidateJson[];
}

/** One review row, with the checkbox state the UI starts from. */
export interface ReviewRow {
	readonly id: string;
	readonly location: string;
	readonly kind: PatternKindLabel;
	readonly masked: string;
	readonly region: string;
	/** Always false: approval must be an explicit human action. */
	readonly checked: false;
}

function isRecord(value: unknown): value is Record<string, unknown> {
	return typeof value === "object" && value !== null;
}

/**
 * Parse and validate a review JSON string.
 *
 * Throws a plain Error naming the malformed field; the message never includes
 * candidate text, so an error cannot become a secret leak either.
 */
export function parseReview(text: string): PatternReviewJson {
	let parsed: unknown;
	try {
		parsed = JSON.parse(text);
	} catch {
		throw new Error("the scan report was not valid JSON");
	}
	if (!isRecord(parsed) || parsed.schema !== 1) {
		throw new Error("the scan report has an unsupported review schema");
	}
	if (!Array.isArray(parsed.candidates)) {
		throw new Error("the scan report has no candidate list");
	}
	const candidates: PatternCandidateJson[] = parsed.candidates.map((item) => {
		if (!isRecord(item)) throw new Error("the scan report has a malformed candidate");
		const { id, page, line, start, end, pattern, masked, region } = item;
		if (
			typeof id !== "string" ||
			id === "" ||
			![page, line, start, end].every((n) => typeof n === "number" && Number.isInteger(n)) ||
			(pattern !== "ssn" && pattern !== "card" && pattern !== "email" && pattern !== "literal") ||
			typeof masked !== "string" ||
			!Array.isArray(region) ||
			region.length !== 4 ||
			!region.every((n) => typeof n === "number" && Number.isFinite(n))
		) {
			throw new Error("the scan report has a malformed candidate");
		}
		return {
			id,
			page: page as number,
			line: line as number,
			start: start as number,
			end: end as number,
			pattern,
			masked,
			region: region as unknown as [number, number, number, number],
		};
	});
	return {
		schema: 1,
		source: typeof parsed.source === "string" ? parsed.source : "",
		source_len: typeof parsed.source_len === "number" ? parsed.source_len : 0,
		source_fnv: typeof parsed.source_fnv === "number" ? parsed.source_fnv : 0,
		candidates,
	};
}

/** Pull the `-review.json` report out of a scan ToolResult and parse it. */
export function reviewFromResult(result: ToolResult): PatternReviewJson {
	const index = result.reports.findIndex(
		(report) => report.kind === "report" && report.name.toLowerCase().endsWith("-review.json"),
	);
	if (index < 0) {
		throw new Error("the scan produced no review report");
	}
	const output = result.outputs[index];
	if (output === undefined) {
		throw new Error("the scan report has no bytes");
	}
	return parseReview(new TextDecoder().decode(output.bytes));
}

/** The exact status copy, including the no-auto-apply promise. */
export function reviewStatus(count: number): string {
	return `${count} candidate${count === 1 ? "" : "s"} found. Select which to redact — nothing is redacted until you apply.`;
}

/** Turn a masked review into checkbox rows, all initially unchecked. */
export function reviewRows(review: PatternReviewJson): ReviewRow[] {
	return review.candidates.map((candidate) => ({
		id: candidate.id,
		location: `page ${candidate.page + 1}, line ${candidate.line + 1}`,
		kind: candidate.pattern,
		masked: candidate.masked,
		region: candidate.region.map((n) => n.toFixed(1)).join(", "),
		checked: false,
	}));
}

/**
 * Resolve a user's checkbox selection to the explicit approval id list.
 * `isChecked` is supplied by the DOM layer; no row is approved by default, and
 * only ids present in the review can be approved (a stale id is ignored here
 * and would be refused by the guest anyway).
 */
export function approvedIdsFrom(
	rows: readonly ReviewRow[],
	isChecked: (id: string) => boolean,
): string[] {
	const known = new Set(rows.map((row) => row.id));
	return rows.filter((row) => known.has(row.id) && isChecked(row.id)).map((row) => row.id);
}

/** The result rows for the apply phase: measured counts and honest verdict. */
export function redactPatternsReportRows(verification: Verification): VerificationRow[] {
	return [
		{
			label: "Pages",
			value:
				verification.pages_in === undefined
					? String(verification.pages_out)
					: `${verification.pages_in} → ${verification.pages_out}`,
		},
		{
			label: "Size",
			value: `${formatBytes(verification.bytes_in)} → ${formatBytes(verification.bytes_out)}`,
		},
		{ label: "Verification", value: verification.verdict },
		{ label: "Read-back limit", value: REDACT_VERIFY_NOTE },
	];
}
