import { describe, expect, it } from "vitest";
import {
	REQUEST_MAGIC,
	RESPONSE_MAGIC,
	ToolProtocolError,
	type Verification,
	countPdfPages,
	decodeToolResponse,
	encodeToolRequest,
	formatBytes,
	verificationRows,
} from "./protocol.js";

function u32(value: number): number[] {
	return [value & 0xff, (value >>> 8) & 0xff, (value >>> 16) & 0xff, (value >>> 24) & 0xff];
}

function text(value: string): number[] {
	return Array.from(new TextEncoder().encode(value));
}

function block(bytes: number[]): number[] {
	return [...u32(bytes.length), ...bytes];
}

function response(
	status: number,
	message: string,
	outputs: Array<{ name: string; bytes: number[] }>,
): Uint8Array {
	const bytes = [
		...u32(RESPONSE_MAGIC),
		...u32(status),
		...block(text(message)),
		...u32(outputs.length),
	];
	for (const output of outputs) {
		bytes.push(...block(text(output.name)), ...block(output.bytes));
	}
	return new Uint8Array(bytes);
}

function readU32(bytes: Uint8Array, offset: number): number {
	return (
		bytes[offset]! |
		(bytes[offset + 1]! << 8) |
		(bytes[offset + 2]! << 16) |
		(bytes[offset + 3]! << 24)
	);
}

const verification: Verification = {
	pages_in: 2,
	pages_out: 2,
	annotations_in: 0,
	annotations_out: 0,
	fields_in: 1,
	fields_out: 1,
	ocgs_in: 0,
	ocgs_out: 0,
	outline_entries_in: 0,
	outline_entries_out: 0,
	embedded_files_in: 0,
	embedded_files_out: 0,
	bytes_in: 2048,
	bytes_out: 1024,
	verdict: "ok",
	checked: ["pages", "annotations", "fields", "ocgs", "outline_entries", "embedded_files"],
};

describe("encodeToolRequest", () => {
	it("writes the documented little-endian layout", () => {
		const encoded = encodeToolRequest({
			op: "split",
			options: { from: 0, to: 1 },
			files: [{ name: "a.pdf", bytes: new Uint8Array([0x25, 0x50, 0x44, 0x46, 0x2d]) }],
		});
		let offset = 0;
		expect(readU32(encoded, offset)).toBe(REQUEST_MAGIC);
		offset += 4;
		expect(readU32(encoded, offset)).toBe(1);
		offset += 4;
		expect(readU32(encoded, offset)).toBe(2);
		offset += 4;
		const optionsLength = readU32(encoded, offset);
		offset += 4;
		expect(new TextDecoder().decode(encoded.subarray(offset, offset + optionsLength))).toBe(
			'{"from":0,"to":1}',
		);
		offset += optionsLength;
		expect(readU32(encoded, offset)).toBe(1);
		offset += 4;
		const nameLength = readU32(encoded, offset);
		offset += 4;
		expect(new TextDecoder().decode(encoded.subarray(offset, offset + nameLength))).toBe("a.pdf");
		offset += nameLength;
		expect(readU32(encoded, offset)).toBe(5);
		offset += 4;
		expect(Array.from(encoded.subarray(offset))).toEqual([0x25, 0x50, 0x44, 0x46, 0x2d]);
	});

	it("encodes the attach and detach operations at 20 and 21", () => {
		const files = [
			{ name: "a.pdf", bytes: new Uint8Array([0x25]) },
			{ name: "note.txt", bytes: new Uint8Array([0x68]) },
		];
		expect(readU32(encodeToolRequest({ op: "attach", files }), 8)).toBe(20);
		expect(readU32(encodeToolRequest({ op: "detach", files }), 8)).toBe(21);
	});

	it("encodes repair at 22", () => {
		const files = [{ name: "a.pdf", bytes: new Uint8Array([0x25]) }];
		expect(readU32(encodeToolRequest({ op: "repair", files }), 8)).toBe(22);
	});

	it("encodes the metadata report at 27", () => {
		const files = [{ name: "a.pdf", bytes: new Uint8Array([0x25]) }];
		expect(readU32(encodeToolRequest({ op: "metadata_report", files }), 8)).toBe(27);
	});

	it("encodes the fingerprint report at 31", () => {
		const files = [{ name: "a.pdf", bytes: new Uint8Array([0x25]) }];
		expect(readU32(encodeToolRequest({ op: "fingerprint", files }), 8)).toBe(31);
	});

	it("encodes the privacy scan at 32", () => {
		const files = [{ name: "a.pdf", bytes: new Uint8Array([0x25]) }];
		expect(readU32(encodeToolRequest({ op: "privacy", files }), 8)).toBe(32);
	});

	it("encodes the batch queue at 28", () => {
		const files = [
			{ name: "a.pdf", bytes: new Uint8Array([0x25]) },
			{ name: "b.pdf", bytes: new Uint8Array([0x25]) },
		];
		expect(readU32(encodeToolRequest({ op: "batch", options: { tool: "compress" }, files }), 8)).toBe(
			28,
		);
	});

	it("rejects an empty file list and malformed operations before calling the guest", () => {
		expect(() => encodeToolRequest({ op: "merge", files: [] })).toThrow(ToolProtocolError);
		expect(() =>
			encodeToolRequest({
				op: "not-a-tool" as never,
				files: [{ name: "a.pdf", bytes: new Uint8Array() }],
			}),
		).toThrow(/unknown tool operation/);
	});
});

describe("decodeToolResponse", () => {
	it("decodes a success response with one output and verification data", () => {
		const result = decodeToolResponse(
			response(0, JSON.stringify(verification), [
				{ name: "out.pdf", bytes: [0x25, 0x50, 0x44, 0x46] },
			]),
		);
		expect(result.outputs[0]?.name).toBe("out.pdf");
		expect(Array.from(result.outputs[0]?.bytes ?? [])).toEqual([0x25, 0x50, 0x44, 0x46]);
		expect(result.verification.verdict).toBe("ok");
		expect(result.reports).toHaveLength(1);
		expect(result.reports[0]?.kind).toBe("pdf");
		expect(result.reports[0]?.pages).toBe(2);
		expect(result.reports[0]?.bytes).toBe(4);
	});

	it("keeps every output and its per-file report", () => {
		const message = {
			...verification,
			pages_out: 5,
			bytes_out: 19,
			outputs: [
				{ name: "doc-pages-1-2.pdf", kind: "pdf", pages: 2, bytes: 6, verdict: "ok" },
				{
					name: "doc-pages-3-5.pdf",
					kind: "pdf",
					pages: 3,
					bytes: 8,
					verdict: "ok",
					overTarget: true,
				},
				{ name: "doc-split.zip", kind: "zip", pages: 0, bytes: 5, verdict: "bundle" },
			],
		};
		const result = decodeToolResponse(
			response(0, JSON.stringify(message), [
				{ name: "doc-pages-1-2.pdf", bytes: [1, 2, 3, 4, 5, 6] },
				{ name: "doc-pages-3-5.pdf", bytes: [7, 8, 9, 10, 11, 12, 13, 14] },
				{ name: "doc-split.zip", bytes: [15, 16, 17, 18, 19] },
			]),
		);
		expect(result.outputs.map((output) => output.name)).toEqual([
			"doc-pages-1-2.pdf",
			"doc-pages-3-5.pdf",
			"doc-split.zip",
		]);
		expect(result.reports.map((report) => report.kind)).toEqual(["pdf", "pdf", "zip"]);
		expect(result.reports.map((report) => report.pages)).toEqual([2, 3, 0]);
		expect(result.reports.map((report) => report.overTarget)).toEqual([false, true, false]);
	});

	it("accepts a read-only report output as its own kind", () => {
		const message = {
			...verification,
			pages_in: 3,
			pages_out: 3,
			bytes_in: 4096,
			bytes_out: 4096,
			verdict: "read",
			outputs: [{ name: "doc-metadata.json", kind: "report", pages: 0, bytes: 2, verdict: "read" }],
		};
		const result = decodeToolResponse(
			response(0, JSON.stringify(message), [{ name: "doc-metadata.json", bytes: [0x7b, 0x7d] }]),
		);
		expect(result.reports[0]?.kind).toBe("report");
		expect(result.reports[0]?.pages).toBe(0);
		expect(result.verification.verdict).toBe("read");
	});

	it("keeps a batch row for every file, including the one that failed", () => {
		const batch = {
			tool: "compress",
			total: 3,
			ok: 2,
			failed: 1,
			files: [
				{
					input: "alpha.pdf",
					name: "alpha-compressed.pdf",
					status: "ok",
					pages: 2,
					bytes: 6,
					verdict: "ok",
				},
				{
					input: "beta.pdf",
					name: "beta-compressed.pdf",
					status: "ok",
					pages: 3,
					bytes: 7,
					verdict: "ok",
				},
				{ input: "broken.pdf", name: null, status: "failed", error: "in/2: not a PDF" },
			],
		};
		const message = {
			...verification,
			pages_out: 5,
			verdict: "partial",
			outputs: [
				{ name: "alpha-compressed.pdf", kind: "pdf", pages: 2, bytes: 6, verdict: "ok" },
				{ name: "beta-compressed.pdf", kind: "pdf", pages: 3, bytes: 7, verdict: "ok" },
				{ name: "alpha-batch.zip", kind: "zip", pages: 0, bytes: 2, verdict: "bundle" },
			],
			batch,
		};
		const result = decodeToolResponse(
			response(0, JSON.stringify(message), [
				{ name: "alpha-compressed.pdf", bytes: new Array(6).fill(0x25) },
				{ name: "beta-compressed.pdf", bytes: new Array(7).fill(0x25) },
				{ name: "alpha-batch.zip", bytes: [0x50, 0x4b] },
			]),
		);
		expect(result.batch?.tool).toBe("compress");
		expect(result.batch?.ok).toBe(2);
		expect(result.batch?.failed).toBe(1);
		expect(result.batch?.files.map((row) => row.status)).toEqual(["ok", "ok", "failed"]);
		expect(result.batch?.files[2]?.input).toBe("broken.pdf");
		expect(result.batch?.files[2]?.name).toBeNull();
		expect(result.batch?.files[2]?.error).toContain("not a PDF");
		expect(result.outputs).toHaveLength(3);
	});

	it("leaves the batch summary absent for a single-document operation", () => {
		const message = {
			...verification,
			outputs: [{ name: "doc.pdf", kind: "pdf", pages: 2, bytes: 4, verdict: "ok" }],
		};
		const result = decodeToolResponse(
			response(0, JSON.stringify(message), [
				{ name: "doc.pdf", bytes: [0x25, 0x50, 0x44, 0x46] },
			]),
		);
		expect(result.batch).toBeUndefined();
	});

	it("rejects batch data whose counts do not match its rows", () => {
		const message = {
			...verification,
			outputs: [{ name: "doc.pdf", kind: "pdf", pages: 2, bytes: 4, verdict: "ok" }],
			batch: { tool: "compress", total: 2, ok: 2, failed: 0, files: [{ input: "a.pdf", name: "doc.pdf", status: "ok" }] },
		};
		expect(() =>
			decodeToolResponse(
				response(0, JSON.stringify(message), [
					{ name: "doc.pdf", bytes: [0x25, 0x50, 0x44, 0x46] },
				]),
			),
		).toThrow(/mismatched batch data/);
	});

	it("rejects a successful batch row with no download and a failed row with no reason", () => {
		const rowless = {
			...verification,
			outputs: [{ name: "doc.pdf", kind: "pdf", pages: 2, bytes: 4, verdict: "ok" }],
			batch: { tool: "compress", total: 1, ok: 1, failed: 0, files: [{ input: "a.pdf", name: null, status: "ok" }] },
		};
		expect(() =>
			decodeToolResponse(
				response(0, JSON.stringify(rowless), [
					{ name: "doc.pdf", bytes: [0x25, 0x50, 0x44, 0x46] },
				]),
			),
		).toThrow(/must name its download/);

		const reasonless = {
			...verification,
			outputs: [{ name: "doc.pdf", kind: "pdf", pages: 2, bytes: 4, verdict: "ok" }],
			batch: {
				tool: "compress",
				total: 2,
				ok: 1,
				failed: 1,
				files: [
					{ input: "a.pdf", name: "doc.pdf", status: "ok", pages: 2, bytes: 4, verdict: "ok" },
					{ input: "b.pdf", name: null, status: "failed" },
				],
			},
		};
		expect(() =>
			decodeToolResponse(
				response(0, JSON.stringify(reasonless), [
					{ name: "doc.pdf", bytes: [0x25, 0x50, 0x44, 0x46] },
				]),
			),
		).toThrow(/must say why/);
	});

	it("rejects a successful batch row that does not report its own result", () => {
		const message = {
			...verification,
			outputs: [{ name: "doc.pdf", kind: "pdf", pages: 2, bytes: 4, verdict: "ok" }],
			batch: {
				tool: "compress",
				total: 1,
				ok: 1,
				failed: 0,
				files: [{ input: "a.pdf", name: "doc.pdf", status: "ok", pages: -1 }],
			},
		};
		expect(() =>
			decodeToolResponse(
				response(0, JSON.stringify(message), [
					{ name: "doc.pdf", bytes: [0x25, 0x50, 0x44, 0x46] },
				]),
			),
		).toThrow(/must report its own result/);
	});

	it("rejects an unknown output kind", () => {
		const message = {
			...verification,
			outputs: [{ name: "doc.pdf", kind: "exe", pages: 2, bytes: 4, verdict: "ok" }],
		};
		expect(() =>
			decodeToolResponse(
				response(0, JSON.stringify(message), [
					{ name: "doc.pdf", bytes: [0x25, 0x50, 0x44, 0x46] },
				]),
			),
		).toThrow(/unknown output kind/);
	});

	it("keeps attachment files distinct from PDFs and the ZIP bundle", () => {
		const message = {
			...verification,
			pages_in: undefined,
			pages_out: 0,
			bytes_out: 17,
			checked: ["embedded_files"],
			embedded_files_in: 2,
			embedded_files_out: 2,
			outputs: [
				{ name: "notes.txt", kind: "file", pages: 0, bytes: 6, verdict: "extracted" },
				{ name: "evil.txt", kind: "file", pages: 0, bytes: 4, verdict: "extracted" },
				{ name: "bundle-attachments.zip", kind: "zip", pages: 0, bytes: 7, verdict: "bundle" },
			],
		};
		const result = decodeToolResponse(
			response(0, JSON.stringify(message), [
				{ name: "notes.txt", bytes: [1, 2, 3, 4, 5, 6] },
				{ name: "evil.txt", bytes: [7, 8, 9, 10] },
				{ name: "bundle-attachments.zip", bytes: [11, 12, 13, 14, 15, 16, 17] },
			]),
		);
		expect(result.reports.map((report) => report.kind)).toEqual(["file", "file", "zip"]);
		expect(result.reports.map((report) => report.pages)).toEqual([0, 0, 0]);
		expect(result.verification.embedded_files_in).toBe(2);
		expect(result.verification.embedded_files_out).toBe(2);
	});

	it("rejects a non-boolean overTarget flag", () => {
		const message = {
			...verification,
			outputs: [
				{
					name: "out.pdf",
					kind: "pdf",
					pages: 2,
					bytes: 4,
					verdict: "ok",
					overTarget: "yes",
				},
			],
		};
		expect(() =>
			decodeToolResponse(
				response(0, JSON.stringify(message), [
					{ name: "out.pdf", bytes: [0x25, 0x50, 0x44, 0x46] },
				]),
			),
		).toThrow(/malformed output metadata/);
	});

	it("rejects output metadata that does not match the wire outputs", () => {
		const message = {
			...verification,
			outputs: [{ name: "other.pdf", kind: "pdf", pages: 2, bytes: 4, verdict: "ok" }],
		};
		expect(() =>
			decodeToolResponse(
				response(0, JSON.stringify(message), [
					{ name: "out.pdf", bytes: [0x25, 0x50, 0x44, 0x46] },
				]),
			),
		).toThrow(/metadata/);
	});

	it("surfaces a guest error message", () => {
		expect(() => decodeToolResponse(response(1, "split needs a from page", []))).toThrow(
			/split needs a from page/,
		);
	});

	it("rejects a bad magic, trailing bytes, and an empty output list", () => {
		const wrongMagic = response(0, JSON.stringify(verification), [{ name: "out.pdf", bytes: [] }]);
		wrongMagic[0] = 0;
		expect(() => decodeToolResponse(wrongMagic)).toThrow(/unexpected response/);

		const withTrailing = response(0, JSON.stringify(verification), [
			{ name: "out.pdf", bytes: [] },
		]);
		const trailing = new Uint8Array(withTrailing.length + 1);
		trailing.set(withTrailing);
		expect(() => decodeToolResponse(trailing)).toThrow(/trailing bytes/);

		expect(() => decodeToolResponse(response(0, JSON.stringify(verification), []))).toThrow(
			/no output/,
		);
	});
});

describe("verificationRows", () => {
	it("shows measured counts and the verdict", () => {
		const rows = verificationRows(verification);
		expect(rows.find((row) => row.label === "Pages")?.value).toBe("2 → 2");
		expect(rows.find((row) => row.label === "Form fields")?.value).toBe("1 → 1");
		expect(rows.find((row) => row.label === "Size")?.value).toBe("2.0 KiB → 1.0 KiB");
		expect(rows.at(-1)).toEqual({ label: "Verification", value: "ok" });
	});

	it("shows the recovery strategy only when one is reported", () => {
		const plain = verificationRows(verification);
		expect(plain.some((row) => row.label === "Recovery")).toBe(false);
		const recovered = verificationRows({
			...verification,
			repair_mode: "reconstructed index",
		});
		expect(recovered.find((row) => row.label === "Recovery")?.value).toBe("reconstructed index");
	});
});

describe("formatBytes", () => {
	it("formats byte counts without lying about negative or invalid values", () => {
		expect(formatBytes(512)).toBe("512 B");
		expect(formatBytes(2048)).toBe("2.0 KiB");
		expect(formatBytes(1024 * 1024)).toBe("1.0 MiB");
		expect(formatBytes(-1)).toBe("—");
		expect(formatBytes(Number.NaN)).toBe("—");
	});
});

describe("countPdfPages", () => {
	function pdf(lines: string[]): Uint8Array {
		return new Uint8Array(new TextEncoder().encode(lines.join("\n")));
	}

	it("counts leaf /Page marks and ignores /Pages", () => {
		const bytes = pdf([
			"1 0 obj <</Type /Catalog /Pages 2 0 R>> endobj",
			"2 0 obj <</Type /Pages /Count 3 /Kids [3 0 R 4 0 R 5 0 R]>> endobj",
			"3 0 obj <</Type /Page /Parent 2 0 R>> endobj",
			"4 0 obj <</Type/Page/Parent 2 0 R>> endobj",
			"5 0 obj <</Type	/Page /Parent 2 0 R>> endobj",
		]);
		expect(countPdfPages(bytes)).toBe(3);
	});

	it("yields 0 for non-PDF bytes and compressed documents", () => {
		expect(countPdfPages(new Uint8Array([0xff, 0xd8, 0xff, 0x00]))).toBe(0);
		expect(countPdfPages(new Uint8Array(0))).toBe(0);
		// An object stream hides the leaves; the count function must say
		// "unknown" (0) rather than guess.
		expect(countPdfPages(pdf(["<</Type /ObjStm>>"]))).toBe(0);
	});

	it("does not count longer name keys that merely start with Page", () => {
		const bytes = pdf(["<</PageMode /UseOutlines /PageOrigin [0 0]>>", "<</Type /PageObject>>"]);
		expect(countPdfPages(bytes)).toBe(0);
	});
});
