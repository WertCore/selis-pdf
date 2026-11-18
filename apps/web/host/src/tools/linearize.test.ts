/**
 * The linearize card's presentation helpers, tested without a browser.
 *
 * These are read-back checks on the facts parsed from a produced PDF and on
 * the rows the card builds from those facts plus the engine's verification —
 * not string matches against source files.
 */

import { describe, expect, it } from "vitest";
import type { Verification } from "./protocol.js";
import { LINEARIZE_NOTE, linearizeReportRows, readLinearization } from "./linearize.js";

/** A small, well-formed linearised two-page PDF with a binary hint stream. */
function linearizedPdf(length = 1234, pages = 2): Uint8Array {
	const parts = [
		"%PDF-1.4\n",
		"1 0 obj\n<< /Linearized 1 /L " + length + " /H [ 600 100 ] /O 3 /E 900 /N " + pages + " /T 511 /P 1 >>\nendobj\n",
		"% padding\n",
		"2 0 obj\n<< /Type /Catalog >>\nendobj\n",
		"3 0 obj\n<< /Type /Page /Contents 5 0 R >>\nendobj\n",
		"4 0 obj\n<< /Type /Pages /Kids [3 0 R 5 0 R] >>\nendobj\n",
		"5 0 obj\n<< /Type /Page /Contents 6 0 R >>\nendobj\n",
		"6 0 obj\n<< >>\nstream\nBT (one) Tj ET\nendstream\nendobj\n",
		// Text that looks like an object header lives inside a stream and must
		// not be counted as a real object.
		"7 0 obj\n<< >>\nstream\nBT (8 0 obj /Type /Page) Tj ET\nendstream\nendobj\n",
		"8 0 obj\n<< /S 10 >>\nstream\n\x00\x01\x02 binary \xff\xfe bytes\nendstream\nendobj\n",
	];
	const text = parts.join("");
	return Uint8Array.from(text, (ch) => ch.charCodeAt(0) & 0xff);
}

const verification: Verification = {
	pages_in: 2,
	pages_out: 2,
	annotations_in: 0,
	annotations_out: 0,
	fields_in: 0,
	fields_out: 0,
	ocgs_in: 0,
	ocgs_out: 0,
	outline_entries_in: 0,
	outline_entries_out: 0,
	embedded_files_in: 0,
	embedded_files_out: 0,
	bytes_in: 23_136,
	bytes_out: 4_096,
	verdict: "ok",
	checked: ["pages"],
};

describe("the linearize option note", () => {
	it("states the layout, the local-only guarantee and the qpdf limit", () => {
		expect(LINEARIZE_NOTE).toContain("/Linearized");
		expect(LINEARIZE_NOTE).toContain("hint tables");
		expect(LINEARIZE_NOTE).toContain("never uploaded");
		expect(LINEARIZE_NOTE).toContain("not a qpdf --check-linearization verdict");
	});
});

describe("readLinearization", () => {
	it("reads the lin dictionary and the real page objects from the output bytes", () => {
		const facts = readLinearization(linearizedPdf());
		expect(facts.linearized).toBe(true);
		expect(facts.length).toBe(1234);
		expect(facts.pages).toBe(2);
		expect(facts.firstPageObject).toBe(3);
		expect(facts.lastPageObject).toBe(5);
		// Eight real object headers; the "8 0 obj" inside a stream is skipped.
		expect(facts.objectCount).toBe(8);
	});

	it("derives every value from the bytes, not fixed copy", () => {
		const facts = readLinearization(linearizedPdf(9000, 4));
		expect(facts.length).toBe(9000);
		expect(facts.pages).toBe(4);
	});

	it("degrades honestly on a non-linearised PDF", () => {
		const plain = Uint8Array.from("%PDF-1.4\n1 0 obj\n<< /Type /Catalog >>\nendobj\n", (ch) =>
			ch.charCodeAt(0),
		);
		const facts = readLinearization(plain);
		expect(facts.linearized).toBe(false);
		expect(facts.length).toBeNull();
		expect(facts.pages).toBeNull();
		expect(facts.firstPageObject).toBeNull();
		expect(facts.objectCount).toBe(1);
	});
});

describe("linearizeReportRows", () => {
	it("reports /L, /N, both page objects, the object count, size and verdict", () => {
		const rows = linearizeReportRows(linearizedPdf(), verification);
		const find = (label: string) => rows.find((row) => row.label === label)?.value;
		expect(find("File length /L")).toBe("1234 B (1.2 KiB)");
		expect(find("Pages /N")).toBe("2");
		expect(find("First page object (/O)")).toBe("3");
		expect(find("Last page object")).toBe("5");
		expect(find("Objects")).toBe("8");
		expect(find("Size (before → after)")).toBe("23 KiB → 4.0 KiB");
		expect(find("Structural verdict")).toBe("ok");
	});

	it("does not claim a linearised output when the dictionary is absent", () => {
		const plain = Uint8Array.from("%PDF-1.4\n1 0 obj\n<< /Type /Catalog >>\nendobj\n", (ch) =>
			ch.charCodeAt(0),
		);
		const rows = linearizeReportRows(plain, verification);
		expect(rows).toHaveLength(1);
		expect(rows[0]?.value).toContain("no readable /Linearized dictionary");
	});
});
