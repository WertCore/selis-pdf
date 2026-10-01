import { describe, expect, it } from "vitest";
import {
	type HealthReport,
	type HealthRow,
	healthRows,
	summariseDeviations,
	worstSeverity,
} from "./health.js";

/** A clean, ordinary, untagged document with no deviations. */
function clean(over: Partial<HealthReport> = {}): HealthReport {
	return {
		pages: 1,
		encrypted: false,
		permissions: null,
		tagged: false,
		signature: "absent",
		deviations: [],
		...over,
	};
}

/** The row with this id, or a failure naming what is there instead. */
function row(rows: readonly HealthRow[], id: string): HealthRow {
	const found = rows.find((r) => r.id === id);
	if (found === undefined) {
		throw new Error(`no row ${id}; got ${rows.map((r) => r.id).join(", ")}`);
	}
	return found;
}

describe("healthRows", () => {
	it("renders every field the report carries, so a gap cannot look like a pass", () => {
		// Rule 3. A panel that renders only the fields it likes makes an omitted
		// row and a clean row indistinguishable, and only one of them is true.
		const ids = healthRows(clean({ pages: 12, tagged: true })).map((r) => r.id);
		expect(ids).toEqual(["pages", "encrypted", "tagged", "signature", "deviations"]);
	});

	it("NEVER reports a signature as OK, whatever the input", () => {
		// Rule 1, and the leg that matters most. Nothing here verifies a
		// signature, so a signed document that renders green is the one false
		// thing this panel could possibly say.
		const rows = healthRows(clean({ signature: "present" }));
		expect(row(rows, "signature").severity).not.toBe("ok");
		expect(row(rows, "signature").label).toMatch(/not verified/);
		// And the worst severity of a signed-but-unverified document is a notice.
		expect(worstSeverity(rows)).toBe("notice");
	});

	it("reports untagged as a notice, never a warning", () => {
		// Rule 2. Most PDFs are untagged. Warning on the commonest fact in the
		// format trains a reader to ignore the row that matters.
		const rows = healthRows(clean({ tagged: false }));
		expect(row(rows, "tagged").severity).toBe("notice");
		expect(worstSeverity(rows)).not.toBe("warn");
	});

	it("reports tagged as OK", () => {
		expect(row(healthRows(clean({ tagged: true })), "tagged").severity).toBe("ok");
	});

	it("omits the permissions row for a clear document rather than inventing one", () => {
		// There was no grant to report. Rendering a row would invite the reader
		// to look for a permissions statement the document never made.
		const rows = healthRows(clean({ encrypted: false, permissions: null }));
		expect(rows.map((r) => r.id)).not.toContain("permissions");
	});

	it("names what an encrypted document forbids", () => {
		const rows = healthRows(
			clean({
				encrypted: true,
				// Everything granted.
				permissions: { print: true, modify: true, copy: true, annotate: true },
			}),
		);
		expect(row(rows, "permissions").severity).toBe("ok");
		const denied = healthRows(
			clean({
				encrypted: true,
				permissions: { print: true, modify: false, copy: false, annotate: true },
			}),
		);
		expect(row(denied, "permissions").severity).toBe("notice");
		expect(row(denied, "permissions").label).toContain("editing");
		expect(row(denied, "permissions").label).toContain("copying");
		// And what it ALLOWS is not listed as forbidden.
		expect(row(denied, "permissions").label).not.toContain("printing");
	});

	it("reports a clean document clean", () => {
		// Tagged, or this is not a clean bill of health - see rule 2. An untagged
		// clean document is a `notice`, which is the whole reason this test says
		// "tagged" rather than reusing the bare `clean()` fixture and being
		// surprised.
		expect(worstSeverity(healthRows(clean({ tagged: true })))).toBe("ok");
		// And with nothing else wrong, that is the only difference.
		expect(worstSeverity(healthRows(clean({ tagged: true, signature: "absent" })))).toBe("ok");
	});

	it("reports deviations as the only warning", () => {
		const rows = healthRows(
			clean({
				deviations: [
					{ name: "odd-length-hex", offset: 10 },
					{ name: "odd-length-hex", offset: 20 },
				],
			}),
		);
		expect(worstSeverity(rows)).toBe("warn");
		expect(row(rows, "deviations").label).toContain("2x odd-length-hex");
	});
});

describe("summariseDeviations", () => {
	it("groups by name, so one defect repeated is one problem", () => {
		// 200 rows of the same defect would bury the single thing wrong with the
		// document. Grouping is what keeps the panel readable AND honest.
		const many = Array.from({ length: 200 }, (_, i) => ({ name: "odd-length-hex", offset: i }));
		expect(summariseDeviations(many)).toEqual([{ name: "odd-length-hex", count: 200 }]);
	});

	it("orders by count, then by name, so the panel does not reshuffle", () => {
		const summary = summariseDeviations([
			{ name: "b-thing", offset: 1 },
			{ name: "a-thing", offset: 2 },
			{ name: "a-thing", offset: 3 },
		]);
		expect(summary).toEqual([
			{ name: "a-thing", count: 2 },
			{ name: "b-thing", count: 1 },
		]);
	});

	it("is empty for a clean document", () => {
		expect(summariseDeviations([])).toEqual([]);
	});
});

describe("worstSeverity", () => {
	it("is warn when anything warns, regardless of order", () => {
		const rows: HealthRow[] = [
			{ id: "a", severity: "warn", label: "" },
			{ id: "b", severity: "ok", label: "" },
			{ id: "c", severity: "notice", label: "" },
		];
		expect(worstSeverity(rows)).toBe("warn");
	});

	it("is notice when the worst thing is a notice, and ok when there is nothing", () => {
		expect(
			worstSeverity([
				{ id: "a", severity: "ok", label: "" },
				{ id: "b", severity: "notice", label: "" },
			]),
		).toBe("notice");
		expect(worstSeverity([])).toBe("ok");
		expect(worstSeverity([{ id: "a", severity: "ok", label: "" }])).toBe("ok");
	});
});
