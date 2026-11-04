import { describe, expect, it } from "vitest";
import { ToolProtocolError, type Verification } from "./protocol.js";
import {
	SIGN_PROTOCOL_VERSION,
	SIGN_REQUEST_MAGIC,
	SIGN_RESPONSE_MAGIC,
	SIGN_WASM_URL,
	decodeSignResponse,
	encodeSignRequest,
	signRows,
	signatureRows,
	signingStamp,
} from "./sign-protocol.js";

function u32(value: number): number[] {
	return [value & 0xff, (value >>> 8) & 0xff, (value >>> 16) & 0xff, (value >>> 24) & 0xff];
}

function text(value: string): number[] {
	return Array.from(new TextEncoder().encode(value));
}

function block(bytes: number[]): number[] {
	return [...u32(bytes.length), ...bytes];
}

function readU32(bytes: Uint8Array, offset: number): number {
	return new DataView(bytes.buffer, bytes.byteOffset, bytes.byteLength).getUint32(offset, true);
}

const verification: Verification = {
	pages_in: 2,
	pages_out: 2,
	annotations_out: 1,
	fields_out: 1,
	ocgs_out: 0,
	outline_entries_out: 0,
	embedded_files_out: 0,
	bytes_in: 512,
	bytes_out: 640,
	verdict: "ok",
	checked: ["pages", "ocgs", "outlines", "embedded_files"],
};

const pdfBytes = [0x25, 0x50, 0x44, 0x46];

/** One success envelope with exactly one output document. */
function response(
	message: Record<string, unknown>,
	name: string,
	bytes: number[],
	record?: Record<string, unknown>,
): Uint8Array {
	const outputs =
		record === undefined
			? [{ name, kind: "pdf", pages: 2, bytes: bytes.length, verdict: "ok" }]
			: [record];
	const body = [
		...u32(SIGN_RESPONSE_MAGIC),
		...u32(0),
		...block(text(JSON.stringify({ ...verification, ...message, outputs }))),
		...u32(1),
		...block(text(name)),
		...block(bytes),
	];
	return new Uint8Array(body);
}

/** One success envelope with no output document (the report). */
function reportResponse(report: Record<string, unknown>): Uint8Array {
	const body = [
		...u32(SIGN_RESPONSE_MAGIC),
		...u32(0),
		...block(text(JSON.stringify(report))),
		...u32(0),
	];
	return new Uint8Array(body);
}

describe("encodeSignRequest", () => {
	it("writes the documented little-endian layout with exactly one file", () => {
		const encoded = encodeSignRequest({
			op: "stamp",
			options: { page: 1, lines: [] },
			file: { name: "report.pdf", bytes: new Uint8Array(pdfBytes) },
		});
		expect(readU32(encoded, 0)).toBe(SIGN_REQUEST_MAGIC);
		expect(readU32(encoded, 4)).toBe(SIGN_PROTOCOL_VERSION);
		expect(readU32(encoded, 8)).toBe(1);
		const optionsLength = readU32(encoded, 12);
		const options = new TextDecoder().decode(encoded.slice(16, 16 + optionsLength));
		expect(options).toBe(JSON.stringify({ page: 1, lines: [] }));
		const filesOffset = 16 + optionsLength;
		expect(readU32(encoded, filesOffset)).toBe(1);
		const nameLength = readU32(encoded, filesOffset + 4);
		const name = new TextDecoder().decode(
			encoded.slice(filesOffset + 8, filesOffset + 8 + nameLength),
		);
		expect(name).toBe("report.pdf");
		const dataOffset = filesOffset + 8 + nameLength;
		expect(readU32(encoded, dataOffset)).toBe(pdfBytes.length);
		expect(Array.from(encoded.slice(dataOffset + 4))).toEqual(pdfBytes);
	});

	it("numbers the three operations 1, 2 and 3", () => {
		const file = { name: "a.pdf", bytes: new Uint8Array(pdfBytes) };
		expect(readU32(encodeSignRequest({ op: "stamp", file }), 8)).toBe(1);
		expect(readU32(encodeSignRequest({ op: "sign", file }), 8)).toBe(2);
		expect(readU32(encodeSignRequest({ op: "report", file }), 8)).toBe(3);
	});

	it("carries an absent options block as a zero length", () => {
		const encoded = encodeSignRequest({
			op: "report",
			file: { name: "a.pdf", bytes: new Uint8Array(pdfBytes) },
		});
		expect(readU32(encoded, 12)).toBe(0);
	});

	it("rejects an empty document and an oversized name before the guest sees them", () => {
		expect(() =>
			encodeSignRequest({
				op: "stamp",
				file: { name: "a.pdf", bytes: new Uint8Array(0) },
			}),
		).toThrow(ToolProtocolError);
		expect(() =>
			encodeSignRequest({
				op: "stamp",
				file: { name: `${"n".repeat(128)}.pdf`, bytes: new Uint8Array(pdfBytes) },
			}),
		).toThrow(/too long/);
	});
});

describe("decodeSignResponse", () => {
	it("decodes a stamp and the words that describe it", () => {
		const result = decodeSignResponse(
			response(
				{ operation: "stamp", page: 1, lines: 2, strokes: 1 },
				"report-stamped.pdf",
				pdfBytes,
			),
		);
		expect(result.kind).toBe("document");
		if (result.kind !== "document") {
			throw new Error("expected a document");
		}
		expect(result.report.name).toBe("report-stamped.pdf");
		expect(result.report.kind).toBe("pdf");
		expect(result.output.bytes.length).toBe(pdfBytes.length);
		expect(result.verification.annotations_out).toBe(1);
		expect(result.outcome.operation).toBe("stamp");
		expect(result.outcome.page).toBe(1);
		expect(result.outcome.lines).toBe(2);
		expect(result.outcome.strokes).toBe(1);
		expect(result.outcome.field).toBeUndefined();
	});

	it("decodes a certificate signature by field", () => {
		const result = decodeSignResponse(
			response(
				{
					operation: "sign",
					page: 1,
					field: "Signature1",
					subFilter: "adbe.pkcs7.detached",
				},
				"report-signed.pdf",
				pdfBytes,
			),
		);
		if (result.kind !== "document") {
			throw new Error("expected a document");
		}
		expect(result.outcome.field).toBe("Signature1");
		expect(result.outcome.subFilter).toBe("adbe.pkcs7.detached");
	});

	it("decodes a report with no output at all", () => {
		const result = decodeSignResponse(
			reportResponse({
				status: "ok",
				unsignedFields: 0,
				revocationChecked: false,
				anchorCount: 1,
				signatures: [
					{
						field: "Signature1",
						page: 1,
						subFilter: "adbe.pkcs7.detached",
						signer: "Selis Test Leaf",
						reason: null,
						location: null,
						signingTime: "D:20261104120000Z",
						byteRange: [0, 100, 200, 300],
						coversWholeDocument: true,
						verified: true,
						digest: "SHA-256",
						error: null,
						trust: {
							subject: "Selis Test Leaf",
							issuer: "Selis Test Root",
							trusted: false,
							reason: "the chain does not end at a supplied anchor",
							revocationChecked: false,
							chain: [
								{
									subject: "Selis Test Leaf",
									issuer: "Selis Test Root",
									notBefore: 1,
									notAfter: 2,
									signatureOk: true,
									timeOk: true,
									anchor: false,
								},
							],
						},
					},
				],
			}),
		);
		expect(result.kind).toBe("report");
		if (result.kind !== "report") {
			throw new Error("expected a report");
		}
		expect(result.report.status).toBe("ok");
		expect(result.report.signatures).toHaveLength(1);
		expect(result.report.signatures[0]?.verified).toBe(true);
		expect(result.report.signatures[0]?.trust?.trusted).toBe(false);
		expect(result.report.signatures[0]?.trust?.revocationChecked).toBe(false);
		expect(result.report.anchorCount).toBe(1);
	});

	it("surfaces a guest refusal without inventing one", () => {
		const body = new Uint8Array([
			...u32(SIGN_RESPONSE_MAGIC),
			...u32(1),
			...block(text("a.pdf: this key does not match the certificate; nothing was written")),
			...u32(0),
		]);
		expect(() => decodeSignResponse(body)).toThrow(/does not match/);
	});

	it("does not call a document response without a file a document", () => {
		const body = new Uint8Array([
			...u32(SIGN_RESPONSE_MAGIC),
			...u32(0),
			...block(text(JSON.stringify({ ...verification, operation: "stamp" }))),
			...u32(0),
		]);
		expect(() => decodeSignResponse(body)).toThrow(/malformed signature report/);
	});

	it("rejects a second output", () => {
		const body = new Uint8Array([
			...u32(SIGN_RESPONSE_MAGIC),
			...u32(0),
			...block(text(JSON.stringify({ ...verification, operation: "stamp" }))),
			...u32(2),
			...block(text("a.pdf")),
			...block(pdfBytes),
			...block(text("b.pdf")),
			...block(pdfBytes),
		]);
		expect(() => decodeSignResponse(body)).toThrow(/at most one document/);
	});

	it("rejects a chunk that describes a different output than it wrote", () => {
		const wrongKind = response({ operation: "stamp", page: 1 }, "a-stamped.pdf", pdfBytes, {
			name: "a-stamped.pdf",
			kind: "zip",
			pages: 2,
			bytes: pdfBytes.length,
			verdict: "ok",
		});
		expect(() => decodeSignResponse(wrongKind)).toThrow(/different output/);
	});

	it("rejects a reported size the wire does not carry", () => {
		const tampered = response({ operation: "stamp", page: 1 }, "a-stamped.pdf", pdfBytes, {
			name: "a-stamped.pdf",
			kind: "pdf",
			pages: 2,
			bytes: 2,
			verdict: "ok",
		});
		expect(() => decodeSignResponse(tampered)).toThrow(/malformed output metadata/);
	});

	it("rejects a signature list that is not made of records", () => {
		const body = reportResponse({
			status: "ok",
			unsignedFields: 0,
			revocationChecked: false,
			signatures: [1],
		});
		expect(() => decodeSignResponse(body)).toThrow(/malformed signature entry/);
	});

	it("rejects a truncated body and a foreign magic", () => {
		const good = response({ operation: "stamp", page: 1 }, "a-stamped.pdf", pdfBytes);
		expect(() => decodeSignResponse(good.slice(0, good.length - 4))).toThrow(/ended inside/);
		const withMagic = new Uint8Array(good);
		withMagic[0] = 0x41;
		expect(() => decodeSignResponse(withMagic)).toThrow(/unexpected response/);
	});
});

describe("signRows", () => {
	it("reports the page and the stroke count that were written", () => {
		const stamped = decodeSignResponse(
			response({ operation: "stamp", page: 2, lines: 1, strokes: 3 }, "a-stamped.pdf", pdfBytes),
		);
		if (stamped.kind !== "document") {
			throw new Error("expected a document");
		}
		expect(signRows(stamped)).toEqual([
			{ label: "Page", value: "2" },
			{ label: "Stamp", value: "1 line(s), 3 stroke(s)" },
		]);
	});

	it("names the signature field and its sub-filter", () => {
		const signed = decodeSignResponse(
			response(
				{
					operation: "sign",
					page: 1,
					field: "Signature1",
					subFilter: "adbe.pkcs7.detached",
				},
				"a-signed.pdf",
				pdfBytes,
			),
		);
		if (signed.kind !== "document") {
			throw new Error("expected a document");
		}
		expect(signRows(signed)).toEqual([
			{ label: "Page", value: "1" },
			{ label: "Signature field", value: "Signature1" },
			{ label: "Sub-filter", value: "adbe.pkcs7.detached" },
		]);
	});
});

describe("signatureRows", () => {
	it("says a document with no signature has none", () => {
		const result = decodeSignResponse(
			reportResponse({
				status: "ok",
				unsignedFields: 0,
				revocationChecked: false,
				signatures: [],
			}),
		);
		if (result.kind !== "report") {
			throw new Error("expected a report");
		}
		expect(signatureRows(result.report)).toEqual([
			{ label: "Signatures", value: "none — this document carries no signature" },
			{
				label: "Revocation",
				value:
					"not checked — this build has no network, so no CRL or OCSP response was fetched",
			},
		]);
	});

	it("discloses an unverified signature and an untrusted chain", () => {
		const result = decodeSignResponse(
			reportResponse({
				status: "ok",
				unsignedFields: 0,
				revocationChecked: false,
				signatures: [
					{
						field: "Signature1",
						page: 1,
						signer: "Selis Test Leaf",
						coversWholeDocument: false,
						verified: false,
						error: "the digest does not match the bytes it covers",
						trust: null,
					},
				],
			}),
		);
		if (result.kind !== "report") {
			throw new Error("expected a report");
		}
		const rows = signatureRows(result.report);
		expect(rows.find((row) => row.label === "Signature 1")?.value).toBe(
			"Signature1: NOT verified",
		);
		expect(rows.find((row) => row.label === "Coverage 1")?.value).toBe(
			"part of the document — later bytes are not covered",
		);
		expect(rows.find((row) => row.label === "Problem 1")?.value).toBe(
			"the digest does not match the bytes it covers",
		);
	});
});

describe("the signing chunk path", () => {
	it("fetches the chunk from its own same-origin path", () => {
		expect(SIGN_WASM_URL).toBe("/wasm/selis_pdf_wasm_sign.wasm");
	});
});

describe("signingStamp", () => {
	it("writes UTC YYYYMMDDHHMMSSZ, the form the signature dictionary wants", () => {
		expect(signingStamp(1_700_000_000)).toBe("20231114221320Z");
		expect(signingStamp(0)).toBe("19700101000000Z");
	});

	it("refuses a time that is not whole Unix seconds", () => {
		expect(() => signingStamp(-1)).toThrow(ToolProtocolError);
		expect(() => signingStamp(1.5)).toThrow(/whole number/);
	});
});
