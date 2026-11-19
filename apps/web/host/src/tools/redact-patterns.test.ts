/**
 * The Redact patterns card's review model, tested without a browser.
 *
 * These are checks on the masked data the page can actually see and on the
 * no-auto-apply invariant — not string matches against source files.
 */

import { describe, expect, it } from "vitest";
import type { ToolResult, Verification } from "./protocol.js";
import {
	type PatternReviewJson,
	REDACT_PATTERNS_NOTE,
	REDACT_REVIEW_NOTE,
	REDACT_VERIFY_NOTE,
	approvedIdsFrom,
	parseReview,
	redactPatternsReportRows,
	reviewFromResult,
	reviewRows,
	reviewStatus,
} from "./redact-patterns.js";

const SSN = "123-45-6789";
const CARD = "4111 1111 1111 1111";
const EMAIL = "alice@example.com";

const review: PatternReviewJson = {
	schema: 1,
	source: "in/0",
	source_len: 1024,
	source_fnv: 42,
	candidates: [
		{
			id: "p0-l0-c0",
			page: 0,
			line: 0,
			start: 0,
			end: 11,
			pattern: "ssn",
			masked: "***-**-6789",
			region: [10.04, 20.06, 30, 40],
		},
		{
			id: "p0-l2-c0",
			page: 0,
			line: 2,
			start: 0,
			end: 19,
			pattern: "card",
			masked: "**** **** **** 1111",
			region: [10, 60, 90, 12],
		},
		{
			id: "p0-l4-c0",
			page: 0,
			line: 4,
			start: 0,
			end: 17,
			pattern: "email",
			masked: "a***@example.com",
			region: [10, 100, 80, 12],
		},
	],
};

describe("the option copy", () => {
	it("says the review is mandatory and that nothing is uploaded", () => {
		expect(REDACT_PATTERNS_NOTE).toContain("nothing is redacted until you tick");
		expect(REDACT_PATTERNS_NOTE).toContain("masked text only");
		expect(REDACT_PATTERNS_NOTE).toContain("never uploaded");
		expect(REDACT_REVIEW_NOTE).toContain("unticked row is left");
		expect(REDACT_VERIFY_NOTE).toContain("VERIFY_FAILED");
		expect(REDACT_VERIFY_NOTE).toContain("no file is written");
	});
});

describe("reviewRows", () => {
	it("starts every candidate unchecked — there is no auto-apply", () => {
		const rows = reviewRows(review);
		expect(rows).toHaveLength(3);
		expect(rows.every((row) => row.checked === false)).toBe(true);
	});

	it("shows masked text, a location, a kind and the region", () => {
		const rows = reviewRows(review);
		expect(rows[0]).toMatchObject({
			id: "p0-l0-c0",
			location: "page 1, line 1",
			kind: "ssn",
			masked: "***-**-6789",
			region: "10.0, 20.1, 30.0, 40.0",
		});
		expect(rows[1]?.masked).toBe("**** **** **** 1111");
		expect(rows[2]?.masked).toBe("a***@example.com");
	});

	it("never places a full secret in the rendered model", () => {
		const rendered = JSON.stringify(reviewRows(review));
		for (const secret of [SSN, CARD, EMAIL]) {
			expect(rendered).not.toContain(secret);
		}
		expect(rendered).toContain("***-**-6789");
	});
});

describe("reviewStatus", () => {
	it("states the count and the no-redaction-until-apply promise", () => {
		expect(reviewStatus(3)).toBe(
			"3 candidates found. Select which to redact — nothing is redacted until you apply.",
		);
		expect(reviewStatus(1)).toContain("1 candidate found.");
		expect(reviewStatus(0)).toContain("nothing is redacted until you apply.");
	});
});

describe("parseReview", () => {
	it("accepts a well-formed masked review", () => {
		expect(parseReview(JSON.stringify(review))).toEqual(review);
	});

	it("refuses a wrong schema or a malformed candidate without echoing a secret", () => {
		expect(() => parseReview(JSON.stringify({ ...review, schema: 2 }))).toThrow(
			/unsupported review schema/,
		);
		expect(() => parseReview(JSON.stringify({ ...review, candidates: [{ id: "x" }] }))).toThrow(
			/malformed candidate/,
		);
		let message = "";
		try {
			parseReview(`{"schema":1,"candidates":[{"id":"${SSN}"}]}`);
		} catch (error) {
			message = error instanceof Error ? error.message : "";
		}
		expect(message).not.toContain(SSN);
	});
});

describe("reviewFromResult", () => {
	it("reads the masked review report out of a scan result", () => {
		const bytes = new TextEncoder().encode(JSON.stringify(review));
		const result: ToolResult = {
			verification: {
				pages_out: 0,
				annotations_out: 0,
				fields_out: 0,
				ocgs_out: 0,
				outline_entries_out: 0,
				embedded_files_out: 0,
				bytes_in: 1024,
				bytes_out: 1024,
				verdict: "review",
				checked: [],
			},
			reports: [
				{
					name: "doc-review.json",
					kind: "report",
					pages: 0,
					bytes: bytes.length,
					verdict: "review",
				},
			],
			outputs: [{ name: "doc-review.json", bytes }],
		};
		expect(reviewFromResult(result).candidates).toHaveLength(3);
	});

	it("refuses a scan result that produced no report", () => {
		expect(() =>
			reviewFromResult({
				verification: {} as ToolResult["verification"],
				reports: [],
				outputs: [],
			}),
		).toThrow(/no review report/);
	});
});

describe("approvedIdsFrom", () => {
	it("returns nothing when no checkbox is ticked", () => {
		expect(approvedIdsFrom(reviewRows(review), () => false)).toEqual([]);
	});

	it("returns only the explicitly ticked, known ids", () => {
		const rows = reviewRows(review);
		expect(approvedIdsFrom(rows, (id) => id === "p0-l2-c0")).toEqual(["p0-l2-c0"]);
		expect(approvedIdsFrom(rows, (id) => id === "stale-id")).toEqual([]);
	});
});

describe("redactPatternsReportRows", () => {
	const verification: Verification = {
		pages_in: 2,
		pages_out: 2,
		annotations_out: 0,
		fields_out: 0,
		ocgs_out: 0,
		outline_entries_out: 0,
		embedded_files_out: 0,
		bytes_in: 4096,
		bytes_out: 3072,
		verdict: "ok",
		checked: ["pages"],
	};

	it("reports measured pages, bytes and verdict plus the read-back limit", () => {
		const rows = redactPatternsReportRows(verification);
		expect(rows[0]?.value).toBe("2 → 2");
		expect(rows[1]?.value).toContain("4.0 KiB");
		expect(rows[2]?.value).toBe("ok");
		expect(rows[3]?.value).toContain("VERIFY_FAILED");
	});
});
