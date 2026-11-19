/**
 * The flatten-forms card's presentation helpers, tested without a browser.
 *
 * These are checks on the copy and on the rows built from the engine's own
 * measured verification — not string matches against source files.
 */

import { describe, expect, it } from "vitest";
import { type Verification } from "./protocol.js";
import { FLATTEN_FORMS_NOTE, flattenFormsReportRows } from "./flatten-forms.js";

const verification: Verification = {
	pages_in: 1,
	pages_out: 1,
	annotations_in: 0,
	annotations_out: 0,
	fields_in: 3,
	fields_out: 0,
	ocgs_in: 0,
	ocgs_out: 0,
	outline_entries_in: 0,
	outline_entries_out: 0,
	embedded_files_in: 0,
	embedded_files_out: 0,
	bytes_in: 2048,
	bytes_out: 1536,
	verdict: "ok",
	checked: ["pages", "annotations", "fields", "ocgs", "outline_entries", "embedded_files"],
};

describe("the flatten-forms option note", () => {
	it("names what is baked, the synthesis caveat and the local-only guarantee", () => {
		expect(FLATTEN_FORMS_NOTE).toContain("normal appearance");
		expect(FLATTEN_FORMS_NOTE).toContain("AcroForm");
		expect(FLATTEN_FORMS_NOTE).toContain("appearance synthesis first");
		expect(FLATTEN_FORMS_NOTE).toContain("never uploaded");
	});
});

describe("flattenFormsReportRows", () => {
	it("reports the field count the engine measured", () => {
		const rows = flattenFormsReportRows(verification);
		expect(rows).toHaveLength(1);
		expect(rows[0]?.label).toBe("Interactive fields");
		expect(rows[0]?.value).toContain("3 baked or removed");
	});

	it("derives the value from the verification, not fixed copy", () => {
		const rows = flattenFormsReportRows({ ...verification, fields_in: 2, fields_out: 2 });
		expect(rows[0]?.value).toBe("2 remain");
	});

	it("does not claim a count when the engine reported none", () => {
		const plain: Verification = { ...verification };
		const mutable = plain as { fields_in?: number };
		delete mutable.fields_in;
		const rows = flattenFormsReportRows(plain);
		expect(rows).toHaveLength(1);
		expect(rows[0]?.value).toContain("no field counts");
	});
});
