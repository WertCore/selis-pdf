import { describe, expect, it } from "vitest";
import { ToolProtocolError, type Verification } from "./protocol.js";
import {
	KEY_MATERIAL_BYTES,
	KEY_MATERIAL_HEX_CHARS,
	SECURITY_PROTOCOL_VERSION,
	SECURITY_REQUEST_MAGIC,
	SECURITY_RESPONSE_MAGIC,
	SECURITY_WASM_URL,
	decodeSecurityResponse,
	encodeSecurityRequest,
	randomKeyMaterial,
	securityRows,
} from "./security-protocol.js";

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
	annotations_out: 0,
	fields_out: 0,
	ocgs_out: 0,
	outline_entries_out: 0,
	embedded_files_out: 0,
	bytes_in: 512,
	bytes_out: 640,
	verdict: "ok",
	checked: ["pages"],
};

const pdfBytes = [0x25, 0x50, 0x44, 0x46];

/** One success envelope: the verification, the chunk's own words, and one file. */
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
		...u32(SECURITY_RESPONSE_MAGIC),
		...u32(0),
		...block(text(JSON.stringify({ ...verification, ...message, outputs }))),
		...u32(1),
		...block(text(name)),
		...block(bytes),
	];
	return new Uint8Array(body);
}

describe("encodeSecurityRequest", () => {
	it("writes the documented little-endian layout with exactly one file", () => {
		const encoded = encodeSecurityRequest({
			op: "unlock",
			options: { password: "hunter2" },
			file: { name: "report.pdf", bytes: new Uint8Array(pdfBytes) },
		});
		expect(readU32(encoded, 0)).toBe(SECURITY_REQUEST_MAGIC);
		expect(readU32(encoded, 4)).toBe(SECURITY_PROTOCOL_VERSION);
		expect(readU32(encoded, 8)).toBe(1);
		const optionsLength = readU32(encoded, 12);
		const options = new TextDecoder().decode(encoded.slice(16, 16 + optionsLength));
		expect(options).toBe(JSON.stringify({ password: "hunter2" }));
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
		expect(readU32(encodeSecurityRequest({ op: "unlock", file }), 8)).toBe(1);
		expect(readU32(encodeSecurityRequest({ op: "protect", file }), 8)).toBe(2);
		expect(readU32(encodeSecurityRequest({ op: "clear_permissions", file }), 8)).toBe(3);
	});

	it("carries an absent options block as a zero length", () => {
		const encoded = encodeSecurityRequest({
			op: "unlock",
			file: { name: "a.pdf", bytes: new Uint8Array(pdfBytes) },
		});
		expect(readU32(encoded, 12)).toBe(0);
	});

	it("rejects an empty document and an oversized name before the guest sees them", () => {
		expect(() =>
			encodeSecurityRequest({
				op: "unlock",
				file: { name: "a.pdf", bytes: new Uint8Array(0) },
			}),
		).toThrow(ToolProtocolError);
		expect(() =>
			encodeSecurityRequest({
				op: "unlock",
				file: { name: `${"n".repeat(128)}.pdf`, bytes: new Uint8Array(pdfBytes) },
			}),
		).toThrow(/too long/);
	});
});

describe("decodeSecurityResponse", () => {
	it("decodes one document and the words that describe it", () => {
		const result = decodeSecurityResponse(
			response(
				{
					operation: "protect",
					algorithm: "AES-256 (revision 6)",
					granted: ["print", "copy"],
					denied: ["modify", "annotate"],
					encryptMetadata: true,
				},
				"report-protected.pdf",
				pdfBytes,
			),
		);
		expect(result.report.name).toBe("report-protected.pdf");
		expect(result.report.kind).toBe("pdf");
		expect(result.output.bytes.length).toBe(pdfBytes.length);
		expect(result.verification.pages_out).toBe(2);
		expect(result.outcome.operation).toBe("protect");
		expect(result.outcome.granted).toEqual(["print", "copy"]);
		expect(result.outcome.denied).toEqual(["modify", "annotate"]);
		expect(result.outcome.encryptMetadata).toBe(true);
		expect(result.outcome.encrypted).toBeUndefined();
	});

	it("keeps the unlock answer about whether there was a password at all", () => {
		const removed = decodeSecurityResponse(
			response({ operation: "unlock", encrypted: true }, "a-unlocked.pdf", pdfBytes),
		);
		expect(removed.outcome.encrypted).toBe(true);
		const copied = decodeSecurityResponse(
			response({ operation: "unlock", encrypted: false }, "a-unlocked.pdf", pdfBytes),
		);
		expect(copied.outcome.encrypted).toBe(false);
	});

	it("surfaces a guest refusal without inventing one", () => {
		const body = new Uint8Array([
			...u32(SECURITY_RESPONSE_MAGIC),
			...u32(1),
			...block(text("a.pdf: wrong password (WRONG_PASSWORD); nothing was written")),
			...u32(0),
		]);
		expect(() => decodeSecurityResponse(body)).toThrow(/WRONG_PASSWORD/);
	});

	it("does not call an empty success a document", () => {
		const body = new Uint8Array([
			...u32(SECURITY_RESPONSE_MAGIC),
			...u32(0),
			...block(text(JSON.stringify({ ...verification, operation: "unlock" }))),
			...u32(0),
		]);
		expect(() => decodeSecurityResponse(body)).toThrow(/exactly one document/);
	});

	it("rejects a second output", () => {
		const body = new Uint8Array([
			...u32(SECURITY_RESPONSE_MAGIC),
			...u32(0),
			...block(text(JSON.stringify({ ...verification, operation: "unlock" }))),
			...u32(2),
			...block(text("a.pdf")),
			...block(pdfBytes),
			...block(text("b.pdf")),
			...block(pdfBytes),
		]);
		expect(() => decodeSecurityResponse(body)).toThrow(/exactly one document/);
	});

	it("rejects a chunk that describes a different output than it wrote", () => {
		const wrongKind = response(
			{ operation: "unlock", encrypted: true },
			"a-unlocked.pdf",
			pdfBytes,
			{ name: "a-unlocked.pdf", kind: "zip", pages: 2, bytes: pdfBytes.length, verdict: "ok" },
		);
		expect(() => decodeSecurityResponse(wrongKind)).toThrow(/different output/);
	});

	it("rejects a reported size the wire does not carry", () => {
		const tampered = response(
			{ operation: "clear permissions", previouslyDenied: ["modify"], summary: "s" },
			"a-permissions-cleared.pdf",
			pdfBytes,
			{
				name: "a-permissions-cleared.pdf",
				kind: "pdf",
				pages: 2,
				bytes: 2,
				verdict: "ok",
			},
		);
		expect(() => decodeSecurityResponse(tampered)).toThrow(/malformed output metadata/);
	});

	it("refuses a granted list that is not made of words", () => {
		const body = response({ operation: "protect", granted: [1, 2] }, "a-protected.pdf", pdfBytes);
		expect(() => decodeSecurityResponse(body)).toThrow(/malformed granted list/);
	});

	it("rejects a truncated body and a foreign magic", () => {
		const good = response({ operation: "unlock", encrypted: true }, "a-unlocked.pdf", pdfBytes);
		expect(() => decodeSecurityResponse(good.slice(0, good.length - 4))).toThrow(/ended inside/);
		const withMagic = new Uint8Array(good);
		withMagic[0] = 0x41;
		expect(() => decodeSecurityResponse(withMagic)).toThrow(/unexpected response/);
	});
});

describe("securityRows", () => {
	it("says a plain document was copied rather than unlocked", () => {
		const copied = decodeSecurityResponse(
			response({ operation: "unlock", encrypted: false }, "a-unlocked.pdf", pdfBytes),
		);
		expect(securityRows(copied)).toEqual([
			{
				label: "Password",
				value: "Nothing to remove — this PDF was not password-protected, so its bytes were copied",
			},
		]);
	});

	it("reports the flags that were actually written", () => {
		const protectedDoc = decodeSecurityResponse(
			response(
				{
					operation: "protect",
					algorithm: "AES-256 (revision 6)",
					granted: ["print"],
					denied: ["modify", "copy", "annotate"],
					encryptMetadata: false,
				},
				"a-protected.pdf",
				pdfBytes,
			),
		);
		const rows = securityRows(protectedDoc);
		expect(rows.map((row) => row.label)).toEqual([
			"Encryption",
			"Granted",
			"Restricted",
			"Document metadata",
		]);
		expect(rows.find((row) => row.label === "Restricted")?.value).toBe("modify, copy, annotate");
		expect(rows.find((row) => row.label === "Document metadata")?.value).toBe("left in the clear");
	});

	it("names what the owner password lifted, and admits when nothing was restricted", () => {
		const lifted = decodeSecurityResponse(
			response(
				{
					operation: "clear permissions",
					previouslyDenied: ["modify", "annotate"],
					summary: "R5/6 incremental: /P and /Perms updated",
				},
				"a-permissions-cleared.pdf",
				pdfBytes,
			),
		);
		expect(securityRows(lifted)).toEqual([
			{ label: "Now granted", value: "modify, annotate" },
			{ label: "How", value: "R5/6 incremental: /P and /Perms updated" },
		]);

		const alreadyFree = decodeSecurityResponse(
			response(
				{ operation: "clear permissions", previouslyDenied: [], summary: "nothing to do" },
				"a-permissions-cleared.pdf",
				pdfBytes,
			),
		);
		expect(securityRows(alreadyFree)[0]?.value).toBe(
			"nothing — no permission was restricted before",
		);
	});
});

describe("the page's own entropy", () => {
	it("draws 64 fresh bytes as 128 hex characters", () => {
		expect(KEY_MATERIAL_BYTES).toBe(64);
		const first = randomKeyMaterial();
		const second = randomKeyMaterial();
		expect(first).toHaveLength(KEY_MATERIAL_HEX_CHARS);
		expect(/^[0-9a-f]+$/.test(first)).toBe(true);
		expect(first).not.toBe(second);
	});

	it("fetches the chunk from its own same-origin path", () => {
		expect(SECURITY_WASM_URL).toBe("/wasm/selis_pdf_wasm_security.wasm");
	});
});
