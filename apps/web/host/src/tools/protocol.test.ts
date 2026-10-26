import { describe, expect, it } from "vitest";
import {
	decodeToolResponse,
	encodeToolRequest,
	formatBytes,
	REQUEST_MAGIC,
	RESPONSE_MAGIC,
	ToolProtocolError,
	verificationRows,
	type Verification,
} from "./protocol.js";

function u32(value: number): number[] {
	return [
		value & 0xff,
		(value >>> 8) & 0xff,
		(value >>> 16) & 0xff,
		(value >>> 24) & 0xff,
	];
}

function text(value: string): number[] {
	return Array.from(new TextEncoder().encode(value));
}

function block(bytes: number[]): number[] {
	return [...u32(bytes.length), ...bytes];
}

function response(status: number, message: string, outputs: Array<{ name: string; bytes: number[] }>): Uint8Array {
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
		expect(result.output.name).toBe("out.pdf");
		expect(Array.from(result.output.bytes)).toEqual([0x25, 0x50, 0x44, 0x46]);
		expect(result.verification.verdict).toBe("ok");
	});

	it("surfaces a guest error message", () => {
		expect(() => decodeToolResponse(response(1, "split needs a from page", []))).toThrow(
			/split needs a from page/,
		);
	});

	it("rejects a bad magic, trailing bytes, and an empty output list", () => {
		const wrongMagic = response(0, JSON.stringify(verification), [
			{ name: "out.pdf", bytes: [] },
		]);
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
